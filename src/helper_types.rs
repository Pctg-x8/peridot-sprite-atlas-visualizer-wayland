#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct SafeF32(f32);
// SafeF32 never gets NaN
impl Eq for SafeF32 {}
impl Ord for SafeF32 {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        unsafe { self.partial_cmp(other).unwrap_unchecked() }
    }
}
impl std::hash::Hash for SafeF32 {
    #[inline(always)]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_ne_bytes().hash(state)
    }
}
impl SafeF32 {
    pub const ZERO: Self = Self(0.0);

    pub const unsafe fn new_unchecked(v: f32) -> Self {
        Self(v)
    }

    pub const fn new(v: f32) -> Option<Self> {
        if v.is_nan() {
            None
        } else {
            Some(unsafe { Self::new_unchecked(v) })
        }
    }

    pub const fn value(&self) -> f32 {
        self.0
    }
}
