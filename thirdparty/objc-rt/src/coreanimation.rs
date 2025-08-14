use crate::{
    AsObject, Class, NSObject, Object, Owned, Receiver, Selector,
    corefoundation::{CFTimeInterval, CGFloat},
    foundation::{NSRunLoop, NSRunLoopMode},
};

#[repr(transparent)]
pub struct CADisplayLink(Object);
unsafe impl Sync for CADisplayLink {}
unsafe impl Send for CADisplayLink {}
impl AsObject for CADisplayLink {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for CADisplayLink {}
impl CADisplayLink {
    pub fn new(target: &(impl AsObject + ?Sized), selector: *mut Selector) -> Owned<Self> {
        unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"CADisplayLink")
                    .send2r::<_, _, *mut Object>(
                        Selector::get_cached(c"displayLinkWithTarget:selector:"),
                        target.as_object() as *const _,
                        selector,
                    )
                    .cast::<Self>(),
            )
        }
    }

    #[inline(always)]
    pub fn add_to_run_loop(&self, run_loop: &mut NSRunLoop, mode: NSRunLoopMode) {
        unsafe {
            self.0.send2(
                Selector::get_cached(c"addToRunLoop:forMode:"),
                run_loop.as_object() as *const _,
                (*mode).as_object() as *const _,
            );
        }
    }

    #[inline(always)]
    pub fn timestamp(&self) -> CFTimeInterval {
        unsafe { self.0.send0r(Selector::get_cached(c"timestamp")) }
    }
}

pub trait CALayer: NSObject {
    #[inline(always)]
    fn set_contents_scale(&self, scale: CGFloat) {
        unsafe {
            self.as_object()
                .send1(Selector::get_cached(c"setContentsScale:"), scale);
        }
    }
}

#[repr(transparent)]
pub struct CAMetalLayer(Object);
impl AsObject for CAMetalLayer {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for CAMetalLayer {}
impl CALayer for CAMetalLayer {}
impl CAMetalLayer {
    pub fn new() -> Option<Owned<Self>> {
        unsafe {
            Owned::from_ptr(
                Class::require(c"CAMetalLayer")
                    .send0r::<*mut Object>(Selector::get_cached(c"layer"))
                    .cast::<Self>(),
            )
        }
    }
}
