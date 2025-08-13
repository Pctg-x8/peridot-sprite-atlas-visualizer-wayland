use crate::{
    AsObject, Class, NSObject, Object, Owned, Selector,
    corefoundation::CFTimeInterval,
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
}
impl NSObject for CADisplayLink {}
impl CADisplayLink {
    pub fn new(target: *mut Object, selector: *mut Selector) -> Owned<Self> {
        unsafe {
            Owned::from_ptr_unchecked(Class::require(c"CADisplayLink").send2v(
                Selector::get(c"displayLinkWithTarget:selector:"),
                target,
                selector,
            ))
        }
    }

    #[inline(always)]
    pub fn add_to_run_loop(&self, run_loop: &mut NSRunLoop, mode: NSRunLoopMode) {
        unsafe {
            self.0.send2(
                Selector::get(c"addToRunLoop:forMode:"),
                run_loop.as_object() as *const _,
                (*mode).as_object() as *const _,
            );
        }
    }

    #[inline(always)]
    pub fn timestamp(&self) -> CFTimeInterval {
        unsafe { self.0.send0r(Selector::get(c"timestamp")) }
    }
}
