use bedrock as br;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizePixels {
    pub width: u32,
    pub height: u32,
}
impl From<br::Extent2D> for SizePixels {
    #[inline(always)]
    fn from(value: br::Extent2D) -> Self {
        SizePixels {
            width: value.width,
            height: value.height,
        }
    }
}
impl From<SizePixels> for br::Extent2D {
    #[inline(always)]
    fn from(value: SizePixels) -> Self {
        br::Extent2D {
            width: value.width,
            height: value.height,
        }
    }
}
