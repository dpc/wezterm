use super::*;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

#[test]
fn nine_formats_larger_than_pipe_capacity_can_be_drained_in_reverse_order() {
    let mut readers = vec![];
    for byte in 0..9u8 {
        let (reader, writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let fd: OwnedFd = writer.into();
        // All receives are issued before any read, including the ninth format.
        enqueue(WritePipe::from(fd), Arc::new(vec![byte; 512 * 1024])).unwrap();
        readers.push((reader, byte));
    }
    for (mut reader, byte) in readers.into_iter().rev() {
        let mut data = vec![];
        reader.read_to_end(&mut data).unwrap();
        assert_eq!(data, vec![byte; 512 * 1024]);
    }
    let (mut reader, writer) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let fd: OwnedFd = writer.into();
    enqueue(WritePipe::from(fd), Arc::new(vec![])).unwrap();
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
}

#[test]
fn pending_sends_retain_their_own_source_bytes_across_replacement_and_cancel() {
    let (mut old_reader, old_writer) = UnixStream::pair().unwrap();
    let (mut new_reader, new_writer) = UnixStream::pair().unwrap();
    old_reader
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    new_reader
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let old = Arc::new(vec![1; 512 * 1024]);
    let current = Arc::new(vec![2; 512 * 1024]);
    let fd: OwnedFd = old_writer.into();
    enqueue(WritePipe::from(fd), old.clone()).unwrap();
    drop(old);
    let fd: OwnedFd = new_writer.into();
    enqueue(WritePipe::from(fd), current).unwrap();
    let mut bytes = vec![];
    new_reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, vec![2; 512 * 1024]);
    bytes.clear();
    old_reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, vec![1; 512 * 1024]);
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let fd: OwnedFd = writer.into();
    enqueue(WritePipe::from(fd), Arc::new(vec![3; 512 * 1024])).unwrap();
    // Receiver cancellation is local to that pipe, not clipboard ownership.
    let (mut reader, writer) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let fd: OwnedFd = writer.into();
    enqueue(WritePipe::from(fd), Arc::new(b"still available".to_vec())).unwrap();
    bytes.clear();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"still available");
}
