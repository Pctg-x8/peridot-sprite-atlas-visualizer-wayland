/// https://doc.rust-lang.org/nomicon/ffi.html#representing-opaque-structs
#[macro_export]
macro_rules! FFIOpaqueStruct {
    ($v: vis struct $t: ident) => {
        #[repr(C)]
        $v struct $t {
            _data: [u8; 0],
            _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
        }
    }
}
