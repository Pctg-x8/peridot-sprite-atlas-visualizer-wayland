use std::io::{IoSlice, IoSliceMut};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileMarker {
    Frame = 0,
    Resize = 1,
    PopulateCompositeInstances = 2,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileMarkerCategory {
    Sample = 0,
    Begin = 1,
    End = 2,
}

#[inline(always)]
fn writeva(w: &mut (impl std::io::Write + ?Sized), mut iov: &mut [IoSlice]) -> std::io::Result<()> {
    // strip empty heads
    IoSlice::advance_slices(&mut iov, 0);

    while !iov.is_empty() {
        let b = w.write_vectored(iov)?;
        IoSlice::advance_slices(&mut iov, b);
    }

    Ok(())
}

#[inline(always)]
fn readva(
    r: &mut (impl std::io::Read + ?Sized),
    mut iov: &mut [IoSliceMut],
) -> std::io::Result<()> {
    // strip empty heads
    IoSliceMut::advance_slices(&mut iov, 0);

    while !iov.is_empty() {
        let b = r.read_vectored(iov)?;
        if b == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "no read",
            ));
        }
        IoSliceMut::advance_slices(&mut iov, b);
    }

    Ok(())
}

#[inline(always)]
fn iovm<'v, T>(v: &'v mut T) -> IoSliceMut<'v> {
    IoSliceMut::new(unsafe {
        core::slice::from_raw_parts_mut(v as *mut _ as *mut u8, core::mem::size_of::<T>())
    })
}

#[inline]
pub fn write_file_head(
    w: &mut (impl std::io::Write + ?Sized),
    ts_freq: u64,
) -> std::io::Result<()> {
    writeva(
        w,
        &mut [
            IoSlice::new(&(0x12345678u32.to_ne_bytes())),
            IoSlice::new(&(ts_freq.to_ne_bytes())),
        ],
    )
}

/// return: Some((is_native_endian, ts_freq)) if valid, otherwise None
#[inline]
pub fn validate_file_head(
    r: &mut (impl std::io::Read + ?Sized),
) -> std::io::Result<Option<(bool, u64)>> {
    let mut byte_order_mark = 0u32;
    let mut ts_freq = 0u64;
    readva(r, &mut [iovm(&mut byte_order_mark), iovm(&mut ts_freq)])?;

    if byte_order_mark == 0x12345678 {
        // native endian
        return Ok(Some((true, ts_freq)));
    }
    if byte_order_mark == 0x78563412 {
        // inverted endian
        return Ok(Some((false, ts_freq.swap_bytes())));
    }

    // invalid
    Ok(None)
}

#[inline]
pub fn write_sample_head(
    w: &mut (impl std::io::Write + ?Sized),
    marker: ProfileMarker,
    cat: ProfileMarkerCategory,
    ts: u64,
) -> std::io::Result<()> {
    writeva(
        w,
        &mut [
            IoSlice::new(&[marker as u8, cat as u8]),
            IoSlice::new(&(ts.to_ne_bytes())),
        ],
    )
}

#[inline]
pub fn read_sample_head(
    r: &mut (impl std::io::Read + ?Sized),
    inverted_endian: bool,
) -> std::io::Result<(ProfileMarker, ProfileMarkerCategory, u64)> {
    let mut fixed_bytes = [0u8; 2];
    let mut ts = 0u64;
    readva(r, &mut [IoSliceMut::new(&mut fixed_bytes), iovm(&mut ts)])?;

    Ok((
        unsafe { core::mem::transmute(fixed_bytes[0]) },
        unsafe { core::mem::transmute(fixed_bytes[1]) },
        if inverted_endian { ts.swap_bytes() } else { ts },
    ))
}

#[inline]
pub fn serialize_begin_frame(
    w: &mut (impl std::io::Write + ?Sized),
    ts: u64,
    frame_number: u32,
) -> std::io::Result<()> {
    write_sample_head(w, ProfileMarker::Frame, ProfileMarkerCategory::Begin, ts)?;
    writeva(w, &mut [IoSlice::new(&(frame_number.to_ne_bytes()))])
}
