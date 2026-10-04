use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn pair() -> (UnixMasterPty, UnixSlavePty) {
    let (master, slave) = openpty(PtySize::default()).unwrap();
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave.fd.as_raw_fd(), &mut termios), 0);
        libc::cfmakeraw(&mut termios);
        assert_eq!(
            libc::tcsetattr(slave.fd.as_raw_fd(), libc::TCSANOW, &termios),
            0
        );
    }
    (master, slave)
}

#[test]
fn cancellable_preparation_preserves_blocking_reader_and_delayed_output() {
    let (master, mut slave) = pair();
    let mut reader = master.try_clone_reader().unwrap();
    assert_eq!(
        unsafe { libc::fcntl(master.fd.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK,
        0
    );
    // Prepare before the first read syscall, including already-created fd clones.
    assert!(master.enable_cancellable_writes().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 8];
        let result = reader.read(&mut bytes);
        tx.send((result, bytes)).unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    slave.fd.write_all(b"output").unwrap();
    let (result, bytes) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(result.unwrap(), 6);
    assert_eq!(&bytes[..6], b"output");
}

#[test]
fn cancellation_unblocks_full_input_and_idle_output_without_master_drop() {
    let (master, _slave) = pair();
    master.enable_cancellable_writes().unwrap();
    // Fill the raw slave's input queue without entering a blocking syscall.
    let fill = [b'x'; 8192];
    loop {
        let n = unsafe { libc::write(master.fd.as_raw_fd(), fill.as_ptr().cast(), fill.len()) };
        if n < 0 {
            assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
            break;
        }
    }
    let mut writer = master.take_writer().unwrap();
    let mut reader = master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        tx.send(writer.write_all(&[b'z'; 65536]).map(|_| 1))
            .unwrap()
    });
    std::thread::spawn(move || tx2.send(reader.read(&mut [0; 1])).unwrap());
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    // Explicit pane kill cancellation must work while the master is still alive.
    master.cancel_writes();
    let results = [
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
    ];
    assert!(results.iter().any(|r| matches!(r, Ok(0))));
    assert!(results
        .iter()
        .any(|r| matches!(r, Err(e) if e.kind() == io::ErrorKind::ConnectionAborted)));
}

#[test]
fn master_drop_revokes_clones_and_cancelled_writer_does_not_append_eot() {
    let (master, mut slave) = pair();
    master.enable_cancellable_writes().unwrap();
    let mut writer = master.take_writer().unwrap();
    slave.fd.0.set_non_blocking(true).unwrap();
    drop(master);
    assert_eq!(
        writer.write(b"x").unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
    // Keep the master writer fd alive while checking there are no injected bytes.
    assert_eq!(
        slave.fd.0.read(&mut [0; 8]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(writer);
}

#[test]
fn reader_drains_hup_without_spin_and_empty_read_returns_immediately() {
    let (master, mut slave) = pair();
    master.enable_cancellable_writes().unwrap();
    let mut reader = master.try_clone_reader().unwrap();
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    slave.fd.write_all(b"last").unwrap();
    let mut bytes = [0; 16];
    assert_eq!(reader.read(&mut bytes).unwrap(), 4);
    drop(slave);
    assert_eq!(reader.read(&mut bytes).unwrap(), 0);
}
