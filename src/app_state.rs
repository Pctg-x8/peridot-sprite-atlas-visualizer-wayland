use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::{
    AppUpdateContext, ViewFeedbackContext, coordinate::SizePixels, hittest::HitTestTreeManager,
    peridot,
};

#[derive(Debug)]
pub struct SpriteInfo {
    // immutable
    id: Uuid,
    pub name: String,
    pub source_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub left: u32,
    pub top: u32,
    pub left_slice: u32,
    pub right_slice: u32,
    pub top_slice: u32,
    pub bottom_slice: u32,
    pub selected: bool,
}
impl SpriteInfo {
    pub fn new(name: String, source_path: PathBuf, width: u32, height: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            source_path,
            width,
            height,
            left: 0,
            top: 0,
            left_slice: 0,
            right_slice: 0,
            top_slice: 0,
            bottom_slice: 0,
            selected: false,
        }
    }

    pub const fn id(&self) -> &Uuid {
        &self.id
    }

    pub const fn right(&self) -> u32 {
        self.left + self.width
    }

    pub const fn bottom(&self) -> u32 {
        self.top + self.height
    }
}

pub struct AppState<'a> {
    atlas_size: SizePixels,
    atlas_size_view_feedbacks: Vec<Box<dyn FnMut(&SizePixels) + 'a>>,
    sprites: Vec<SpriteInfo>,
    sprites_view_feedbacks: Vec<Box<dyn FnMut(&[SpriteInfo]) + 'a>>,
    visible_menu: bool,
    visible_menu_view_feedbacks: Vec<
        Box<
            dyn FnMut(
                    &mut ViewFeedbackContext,
                    &mut HitTestTreeManager<AppUpdateContext<'a>>,
                    bool,
                    bool,
                ) + 'a,
        >,
    >,
    current_open_path: Option<PathBuf>,
    current_open_path_view_feedbacks: Vec<Box<dyn FnMut(&Option<PathBuf>) + 'a>>,
}
impl<'a> AppState<'a> {
    pub fn new() -> Self {
        Self {
            atlas_size: SizePixels {
                width: 32,
                height: 32,
            },
            atlas_size_view_feedbacks: Vec::new(),
            sprites: Vec::new(),
            sprites_view_feedbacks: Vec::new(),
            visible_menu: false,
            visible_menu_view_feedbacks: Vec::new(),
            current_open_path: None,
            current_open_path_view_feedbacks: Vec::new(),
        }
    }

    pub fn add_sprites(&mut self, sprites: impl IntoIterator<Item = SpriteInfo>) {
        let mut iter = sprites.into_iter();
        self.sprites.reserve(iter.size_hint().0);
        let mut max_required_size = self.atlas_size;
        while let Some(n) = iter.next() {
            // Power of Twoに丸める（そうするとUV計算が正確になるため）
            max_required_size.width = max_required_size.width.max(n.right()).next_power_of_two();
            max_required_size.height = max_required_size.height.max(n.bottom()).next_power_of_two();

            self.sprites.push(n);
        }

        if max_required_size != self.atlas_size {
            self.atlas_size = max_required_size;
            for cb in self.atlas_size_view_feedbacks.iter_mut() {
                cb(&self.atlas_size);
            }
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }
    }

    pub fn selected_sprites_with_index(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, &SpriteInfo)> {
        self.sprites.iter().enumerate().filter(|(_, x)| x.selected)
    }

    pub fn set_sprite_offset(&mut self, index: usize, left_pixels: u32, top_pixels: u32) {
        let target_sprite = &mut self.sprites[index];
        target_sprite.left = left_pixels;
        target_sprite.top = top_pixels;

        // Sprite Atlasのサイズ調整
        let mut max_required_size = self.atlas_size;
        // Power of Twoに丸める（そうするとUV計算が正確になるため）
        max_required_size.width = max_required_size
            .width
            .max(target_sprite.right())
            .next_power_of_two();
        max_required_size.height = max_required_size
            .height
            .max(target_sprite.bottom())
            .next_power_of_two();
        if max_required_size != self.atlas_size {
            self.atlas_size = max_required_size;
            for cb in self.atlas_size_view_feedbacks.iter_mut() {
                cb(&self.atlas_size);
            }
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }
    }

    pub fn select_sprite(&mut self, index: usize) {
        for (n, x) in self.sprites.iter_mut().enumerate() {
            x.selected = n == index;
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }
    }

    pub fn deselect_sprite(&mut self) {
        for x in self.sprites.iter_mut() {
            x.selected = false;
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }
    }

    pub fn toggle_menu(
        &mut self,
        vf_context: &mut ViewFeedbackContext,
        ht: &mut HitTestTreeManager<AppUpdateContext<'a>>,
    ) {
        self.visible_menu = !self.visible_menu;

        for cb in self.visible_menu_view_feedbacks.iter_mut() {
            cb(vf_context, ht, self.visible_menu, false);
        }
    }

    pub const fn is_visible_menu(&self) -> bool {
        self.visible_menu
    }

    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let mut asset = peridot::SpriteAtlasAsset {
            width: self.atlas_size.width,
            height: self.atlas_size.height,
            sprites: self
                .sprites
                .iter()
                .map(|x| peridot::Sprite {
                    id: x.id.clone(),
                    source_path: x.source_path.clone(),
                    name: x.name.clone(),
                    width: x.width,
                    height: x.height,
                    left: x.left,
                    top: x.top,
                    border_left: x.left_slice,
                    border_top: x.top_slice,
                    border_right: x.right_slice,
                    border_bottom: x.bottom_slice,
                })
                .collect(),
        };
        asset.sprites.sort_by(|a, b| a.id.cmp(&b.id));

        asset.write(
            &mut std::fs::File::options()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?,
        )
    }

    pub fn load(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(), peridot::SpriteAtlasAssetReadError> {
        let asset = peridot::SpriteAtlasAsset::read(&mut std::io::BufReader::new(
            std::fs::File::open(&path)?,
        ))?;

        self.sprites.clear();
        self.sprites
            .extend(asset.sprites.into_iter().map(|x| SpriteInfo {
                id: x.id,
                name: x.name,
                source_path: x.source_path,
                width: x.width,
                height: x.height,
                left: x.left,
                top: x.top,
                left_slice: x.border_left,
                right_slice: x.border_right,
                top_slice: x.border_top,
                bottom_slice: x.border_bottom,
                selected: false,
            }));
        self.atlas_size.width = asset.width;
        self.atlas_size.height = asset.height;
        self.current_open_path = Some(path.as_ref().into());

        for cb in self.atlas_size_view_feedbacks.iter_mut() {
            cb(&self.atlas_size);
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }

        for cb in self.current_open_path_view_feedbacks.iter_mut() {
            cb(&self.current_open_path);
        }

        Ok(())
    }

    // TODO: unregister
    pub fn register_sprites_view_feedback(&mut self, mut fb: impl FnMut(&[SpriteInfo]) + 'a) {
        fb(&self.sprites);
        self.sprites_view_feedbacks.push(Box::new(fb));
    }

    // TODO: unregister
    pub fn register_atlas_size_view_feedback(&mut self, mut fb: impl FnMut(&SizePixels) + 'a) {
        fb(&self.atlas_size);
        self.atlas_size_view_feedbacks.push(Box::new(fb));
    }

    // TODO: unregister
    pub fn register_visible_menu_view_feedback(
        &mut self,
        fb: impl FnMut(
            &mut ViewFeedbackContext,
            &mut HitTestTreeManager<AppUpdateContext<'a>>,
            bool,
            bool,
        ) + 'a,
    ) {
        self.visible_menu_view_feedbacks.push(Box::new(fb));
    }

    // TODO: unregister
    pub fn register_current_open_path_view_feedback(
        &mut self,
        mut fb: impl FnMut(&Option<PathBuf>) + 'a,
    ) {
        fb(&self.current_open_path);
        self.current_open_path_view_feedbacks.push(Box::new(fb));
    }
}
