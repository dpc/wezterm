use super::*;

fn borders(actions: &[Action]) -> usize {
    actions
        .iter()
        .filter(|a| {
            matches!(a,
                Action::DeviceControl(DeviceControlMode::ShortDeviceControl(d))
                    if d.byte == b'b' && d.intermediates == [b'+']
            )
        })
        .count()
}

#[test]
fn cell_border_payload_retention_is_bounded_and_recovers_after_drain() {
    let mut parser = Parser::new();
    parser.parse(b"\x1bP1+b", |_| panic!("partial DCS dispatched"));
    let bytes = [b'x'; 4096];
    for _ in 0..256 {
        parser.parse(&bytes, |_| panic!("oversized DCS dispatched"));
        assert!(parser.state.borrow().dcs.as_ref().unwrap().data.len() <= 256);
    }
    assert_eq!(borders(&parser.parse_as_vec(b"\x1b\\")), 0);
    assert_eq!(
        borders(&parser.parse_as_vec(b"\x1bP1+bcell-border;q\x1b\\")),
        1
    );
}

#[test]
fn cell_border_st_is_required_across_every_split_and_cancellation() {
    let wire = b"\x1bP1+bcell-border;q\x1b\\";
    for split in 0..wire.len() {
        let mut parser = Parser::new();
        assert_eq!(borders(&parser.parse_as_vec(&wire[..split])), 0);
        assert_eq!(borders(&parser.parse_as_vec(&wire[split..])), 1);
    }
    for cancel in [
        b"\x18".as_slice(),
        b"\x1a",
        b"\x1b[H",
        b"\x1b\x1b\\",
        b"\x85",
    ] {
        let mut parser = Parser::new();
        parser.parse(b"\x1bP1+bcell-border;q", |_| {
            panic!("partial DCS dispatched")
        });
        assert_eq!(borders(&parser.parse_as_vec(cancel)), 0);
        assert_eq!(borders(&parser.parse_as_vec(b"\x1b\\")), 0);
    }
    let mut parser = Parser::new();
    let (actions, consumed) = parser.parse_first_as_vec(wire).unwrap();
    assert_eq!(consumed, wire.len());
    assert_eq!(borders(&actions), 1);
}
