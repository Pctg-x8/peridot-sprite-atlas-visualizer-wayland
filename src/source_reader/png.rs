use std::io::{IoSliceMut, Read};

const fn as_mut_u8_slice<T>(sink: &mut T) -> &mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(sink as *mut _ as _, core::mem::size_of::<T>()) }
}

pub struct Metadata {
    pub width: u32,
    pub height: u32,
}
impl Metadata {
    pub fn try_read(reader: &mut (impl Read + ?Sized)) -> Option<Self> {
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf).unwrap();
        if buf != [137, 80, 78, 71, 13, 10, 26, 10] {
            // signature mismatch
            return None;
        }

        // find ihdr
        let mut chunk_data_byte_length = 0u32;
        let mut chunk_type = [0u8; 4];
        let mut readbuf = &mut [
            IoSliceMut::new(as_mut_u8_slice(&mut chunk_data_byte_length)),
            IoSliceMut::new(&mut chunk_type),
        ][..];
        while !readbuf.is_empty() {
            let r = reader.read_vectored(readbuf).unwrap();
            IoSliceMut::advance_slices(&mut readbuf, r);
        }
        let chunk_data_byte_length = u32::from_be(chunk_data_byte_length);
        if chunk_type != *b"IHDR" {
            panic!("invalid png format: no IHDR chunk at head");
        }
        if chunk_data_byte_length < 8 {
            panic!("IHDR chunk is too short");
        }

        let mut width = 0u32;
        let mut height = 0u32;
        let mut readbuf = &mut [
            IoSliceMut::new(as_mut_u8_slice(&mut width)),
            IoSliceMut::new(as_mut_u8_slice(&mut height)),
        ][..];
        while !readbuf.is_empty() {
            let r = reader.read_vectored(readbuf).unwrap();
            IoSliceMut::advance_slices(&mut readbuf, r);
        }

        Some(Self {
            width: u32::from_be(width),
            height: u32::from_be(height),
        })
    }
}
