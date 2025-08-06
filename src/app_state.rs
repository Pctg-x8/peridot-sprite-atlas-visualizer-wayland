use std::{
    ffi::CString,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{coordinate::SizePixels, peridot, source_reader};

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
    pub rotated: bool,
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
            rotated: false,
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

pub struct AppState<'subsystem> {
    atlas_size: SizePixels,
    atlas_size_view_feedbacks: Vec<Box<dyn FnMut(&SizePixels) + 'subsystem>>,
    sprites: Vec<SpriteInfo>,
    sprites_view_feedbacks: Vec<Box<dyn FnMut(&[SpriteInfo]) + 'subsystem>>,
    visible_menu: bool,
    visible_menu_view_feedbacks: Vec<Box<dyn FnMut(bool) + 'subsystem>>,
    current_open_path: Option<PathBuf>,
    current_open_path_view_feedbacks: Vec<Box<dyn FnMut(&Option<PathBuf>) + 'subsystem>>,
}
impl<'subsystem> AppState<'subsystem> {
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

    #[inline]
    pub fn sprites(&self) -> &[SpriteInfo] {
        &self.sprites
    }

    pub fn add_sprites_from_file_paths(
        &mut self,
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) {
        let paths = paths.into_iter();
        let (lb, ub) = paths.size_hint();
        let mut added_sprites = Vec::with_capacity(ub.unwrap_or(lb));
        for path in paths {
            let path = path.as_ref();

            if path.is_dir() {
                // process all files in directory(rec)
                for entry in walkdir::WalkDir::new(&path)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    use crate::app_state::SpriteInfo;

                    let path = entry.path();
                    if !path.is_file() {
                        // 自分自身を含むみたいなのでその場合は見逃す
                        continue;
                    }

                    let mut fs = std::fs::File::open(&path).unwrap();
                    let Some(png_meta) = source_reader::png::Metadata::try_read(&mut fs) else {
                        // PNGじゃないのは一旦見逃す
                        continue;
                    };

                    added_sprites.push(SpriteInfo::new(
                        path.file_stem().unwrap().to_str().unwrap().into(),
                        path.to_path_buf(),
                        png_meta.width,
                        png_meta.height,
                    ));
                }
            } else {
                use crate::app_state::SpriteInfo;

                let mut fs = std::fs::File::open(&path).unwrap();
                let png_meta = match source_reader::png::Metadata::try_read(&mut fs) {
                    Some(x) => x,
                    None => {
                        tracing::warn!(?path, "not a png?");
                        continue;
                    }
                };

                added_sprites.push(SpriteInfo::new(
                    path.file_stem().unwrap().to_str().unwrap().into(),
                    path.to_path_buf(),
                    png_meta.width,
                    png_meta.height,
                ));
            }
        }

        self.add_sprites(added_sprites);
    }

    // TODO: uri_listを別途正しく解析してfrom_file_pathsのほうにまとめたい
    pub fn add_sprites_by_uri_list(&mut self, uris: Vec<CString>) {
        self.add_sprites_from_file_paths(uris.into_iter().filter_map(|x| {
            Some(std::path::PathBuf::from(match x.to_str() {
                Ok(x) => match x.strip_prefix("file://") {
                    Some(x) => x,
                    None => {
                        tracing::warn!(?x, "not started by file://");
                        x
                    }
                },
                Err(e) => {
                    tracing::warn!(reason = ?e, "invalid path");
                    return None;
                }
            }))
        }));
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

    pub fn toggle_menu(&mut self) {
        self.visible_menu = !self.visible_menu;

        for cb in self.visible_menu_view_feedbacks.iter_mut() {
            cb(self.visible_menu);
        }
    }

    pub const fn is_visible_menu(&self) -> bool {
        self.visible_menu
    }

    pub fn arrange(&mut self, allow_rotation: bool) {
        let (mut total_area, mut min_side_require) = (0, 0);
        for x in self.sprites.iter() {
            total_area += (x.width * x.height) as u64;
            min_side_require = min_side_require.max(x.width).max(x.height);
        }
        let suitable_tex_size1 = (total_area as f64).sqrt().ceil() as u32;
        let suitable_tex_width = suitable_tex_size1.max(min_side_require).next_power_of_two();
        let suitable_tex_height = suitable_tex_width;
        println!("suitable atlas size: {suitable_tex_width}px x {suitable_tex_height}px");

        #[derive(Clone)]
        struct VariableLengthBits {
            values: Vec<u64>,
        }
        impl VariableLengthBits {
            pub fn new(init_val: u64) -> Self {
                Self {
                    values: vec![init_val],
                }
            }

            pub fn is_all_one(&self) -> bool {
                self.values.iter().all(|x| x.count_zeros() == 0)
            }

            #[inline(always)]
            const fn index_bitmask(at: usize) -> (usize, u64) {
                (at >> 6, 1u64 << (at & 63))
            }

            pub fn is_one(&self, at: usize) -> bool {
                let (at_index, bitmask) = Self::index_bitmask(at);

                match self.values.get(at_index) {
                    None => false,
                    Some(x) => (x & bitmask) == bitmask,
                }
            }

            pub fn set(&mut self, at: usize) {
                let (at_index, bitmask) = Self::index_bitmask(at);

                self.values.resize(at_index + 1, 0);
                self.values[at_index] |= bitmask;
            }

            pub fn insert_copy_right_bit(&mut self, at: usize) {
                let at_index = at >> 6;
                let bitpos = at & 63;
                let updating_bitmask = !0u64 << bitpos;

                let last_index = self.values.len() - 1;
                if at_index < last_index {
                    // shift later bits
                    // last value
                    let carry = (self.values[last_index] & 0x8000_0000_0000_0000) != 0;
                    self.values[last_index] <<= 1;
                    if carry {
                        self.values.push(0x01);
                    }

                    // shift from back
                    for n in (at_index + 1..self.values.len() - 1).rev() {
                        let carry = (self.values[n] & 0x8000_0000_0000_0000) != 0;
                        self.values[n] <<= 1;
                        if carry {
                            self.values[n + 1] |= 0x01;
                        }
                    }
                }

                let carry = (self.values[at_index] & 0x8000_0000_0000_0000) != 0;
                self.values[at_index] = (self.values[at_index] & !updating_bitmask)
                    | ((self.values[at_index] << 1) & updating_bitmask);
                if carry {
                    if at_index == last_index {
                        self.values.push(0x01);
                    } else {
                        self.values[at_index + 1] |= 0x01;
                    }
                }
            }
        }
        // packing rectangles using dynamically splitted grids: https://www.david-colson.com/2020/03/10/exploring-rect-packing.html
        struct DynamicGrid {
            resident_bitmap: Vec<VariableLengthBits>,
            col_pixels: Vec<u32>,
            row_pixels: Vec<u32>,
        }
        impl DynamicGrid {
            pub fn new(init_width: u32, init_height: u32) -> Self {
                Self {
                    resident_bitmap: vec![VariableLengthBits::new(0)],
                    col_pixels: vec![init_width],
                    row_pixels: vec![init_height],
                }
            }

            #[tracing::instrument(level = tracing::Level::DEBUG, name = "DynamicGrid::try_alloc", skip(self), ret)]
            pub fn try_alloc(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
                // find suitable grids
                let (found, found_grid_index) = 'find_enough_area: {
                    let mut top = 0;
                    for (nr, (&r, rb)) in self
                        .row_pixels
                        .iter()
                        .zip(self.resident_bitmap.iter())
                        .enumerate()
                    {
                        if rb.is_all_one() {
                            // fully resident row
                            top += r;
                            continue;
                        }

                        let mut left = 0;
                        for (nc, &c) in self.col_pixels.iter().enumerate() {
                            if rb.is_one(nc) {
                                // resident col
                                left += c;
                                continue;
                            }

                            let cont_width = c + self
                                .col_pixels
                                .iter()
                                .enumerate()
                                .skip(nc + 1)
                                .take_while(|&(n, _)| !rb.is_one(n))
                                .map(|(_, &c)| c)
                                .sum::<u32>();
                            if cont_width < width {
                                // no suitable width
                                left += c;
                                continue;
                            }

                            let mut cont_height = r;
                            for (&r, rb) in self
                                .row_pixels
                                .iter()
                                .zip(self.resident_bitmap.iter())
                                .skip(nr + 1)
                            {
                                if cont_height >= height {
                                    // enough height
                                    break;
                                }

                                let cont_width1 = self
                                    .col_pixels
                                    .iter()
                                    .enumerate()
                                    .skip(nc)
                                    .take_while(|&(n, _)| !rb.is_one(n))
                                    .map(|(_, &c)| c)
                                    .sum::<u32>();
                                if cont_width1 < cont_width {
                                    // no enough width in next rows
                                    break;
                                }

                                cont_height += r;
                            }

                            if cont_height < height {
                                // no suitable height found
                                left += c;
                                continue;
                            }
                            assert!(cont_width >= width && cont_height >= height); // found enough region

                            break 'find_enough_area ((left, top), (nr, nc));
                        }

                        // not found in row
                        top += r;
                    }

                    // all iteration finished(not found)
                    return None;
                };

                // mark resident / split grid
                let mut height_rest = height;
                let mut filling_nr = found_grid_index.0;
                while height_rest > 0 {
                    let mut r = self.row_pixels[filling_nr];
                    if height_rest < r {
                        // split row
                        let org_nr = filling_nr;
                        let bottom = self.row_pixels[org_nr] - height_rest;
                        self.row_pixels[org_nr] = height_rest;
                        self.row_pixels.insert(org_nr + 1, bottom);
                        // copy resident state
                        self.resident_bitmap
                            .insert(org_nr + 1, self.resident_bitmap[org_nr].clone());
                        r = height_rest;
                    }

                    // fill row
                    let mut width_rest = width;
                    let mut filling_nc = found_grid_index.1;
                    while width_rest > 0 {
                        let mut c = self.col_pixels[filling_nc];
                        if width_rest < c {
                            // split col
                            let org_nc = filling_nc;
                            let right = self.col_pixels[org_nc] - width_rest;
                            self.col_pixels[org_nc] = width_rest;
                            self.col_pixels.insert(org_nc + 1, right);
                            // copy resident state
                            self.resident_bitmap[filling_nr].insert_copy_right_bit(org_nc);
                            c = width_rest;
                        }

                        self.resident_bitmap[filling_nr].set(filling_nc);

                        width_rest -= c;
                        filling_nc += 1;
                    }

                    height_rest -= r;
                    filling_nr += 1;
                }

                Some(found)
            }
        }

        let mut dynamic_grid = DynamicGrid::new(suitable_tex_width, suitable_tex_height);
        for x in self.sprites.iter_mut() {
            // TODO: allow_rotation consideration
            let Some((left, top)) = dynamic_grid.try_alloc(x.width, x.height) else {
                unreachable!("no suitable region(incorrect suitable tex size computation)");
            };

            x.left = left;
            x.top = top;
            x.rotated = false;
        }

        // TODO: この時点でサイズ切り詰められそうなら切り詰める
        self.atlas_size.width = suitable_tex_width;
        self.atlas_size.height = suitable_tex_height;

        for fb in self.atlas_size_view_feedbacks.iter_mut() {
            fb(&self.atlas_size);
        }

        for fb in self.sprites_view_feedbacks.iter_mut() {
            fb(&self.sprites);
        }
    }

    #[tracing::instrument(name = "AppState::save", skip(self), fields(path = %path.as_ref().display()), err(Display))]
    pub fn save(&mut self, path: impl AsRef<Path>) -> std::io::Result<()> {
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
                    rotated: x.rotated,
                })
                .collect(),
        };
        asset.sprites.sort_by(|a, b| a.id.cmp(&b.id));

        asset.write(
            &mut std::fs::File::options()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)?,
        )?;

        self.update_current_open_path(path);
        Ok(())
    }

    #[tracing::instrument(name = "AppState::load", skip(self), fields(path = %path.as_ref().display()), err(Display))]
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
                rotated: false,
                selected: false,
            }));
        self.atlas_size.width = asset.width;
        self.atlas_size.height = asset.height;
        self.update_current_open_path(path);

        for cb in self.atlas_size_view_feedbacks.iter_mut() {
            cb(&self.atlas_size);
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }

        Ok(())
    }

    fn update_current_open_path(&mut self, path: impl AsRef<Path>) {
        self.current_open_path = Some(path.as_ref().into());

        for cb in self.current_open_path_view_feedbacks.iter_mut() {
            cb(&self.current_open_path);
        }
    }

    /// synchronizes views with the state: notifies current state to all view feedback receivers
    pub fn synchronize_view(&mut self) {
        for cb in self.atlas_size_view_feedbacks.iter_mut() {
            cb(&self.atlas_size);
        }

        for cb in self.sprites_view_feedbacks.iter_mut() {
            cb(&self.sprites);
        }

        for cb in self.current_open_path_view_feedbacks.iter_mut() {
            cb(&self.current_open_path);
        }

        for cb in self.visible_menu_view_feedbacks.iter_mut() {
            cb(self.visible_menu);
        }
    }

    // TODO: unregister
    pub fn register_sprites_view_feedback(&mut self, fb: impl FnMut(&[SpriteInfo]) + 'subsystem) {
        self.sprites_view_feedbacks.push(Box::new(fb));
    }

    // TODO: unregister
    pub fn register_atlas_size_view_feedback(
        &mut self,
        mut fb: impl FnMut(&SizePixels) + 'subsystem,
    ) {
        fb(&self.atlas_size);
        self.atlas_size_view_feedbacks.push(Box::new(fb));
    }

    // TODO: unregister
    pub fn register_visible_menu_view_feedback(&mut self, fb: impl FnMut(bool) + 'subsystem) {
        self.visible_menu_view_feedbacks.push(Box::new(fb));
    }

    // TODO: unregister
    pub fn register_current_open_path_view_feedback(
        &mut self,
        fb: impl FnMut(&Option<PathBuf>) + 'subsystem,
    ) {
        self.current_open_path_view_feedbacks.push(Box::new(fb));
    }
}
