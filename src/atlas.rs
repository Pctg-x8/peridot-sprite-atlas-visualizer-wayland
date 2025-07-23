use bedrock as br;

#[derive(Debug, Clone, Copy)]
pub struct AtlasRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}
impl AtlasRect {
    pub const fn width(&self) -> u32 {
        self.right.abs_diff(self.left)
    }

    pub const fn height(&self) -> u32 {
        self.bottom.abs_diff(self.top)
    }

    pub const fn lt_offset(&self) -> br::Offset2D {
        br::Offset2D {
            x: self.left as _,
            y: self.top as _,
        }
    }

    pub const fn extent(&self) -> br::Extent2D {
        br::Extent2D {
            width: self.width(),
            height: self.height(),
        }
    }

    pub const fn vk_rect(&self) -> br::Rect2D {
        self.extent().into_rect(self.lt_offset())
    }

    pub fn vsplit(&mut self, width: u32) -> Self {
        assert!(width <= self.width());

        let r = Self {
            left: self.left + width,
            right: self.right,
            top: self.top,
            bottom: self.bottom,
        };
        self.right = self.left + width;
        r
    }

    pub fn hsplit(&mut self, height: u32) -> Self {
        assert!(height <= self.height());

        let r = Self {
            left: self.left,
            right: self.right,
            top: self.top + height,
            bottom: self.bottom,
        };
        self.bottom = self.top + height;
        r
    }
}

pub struct DynamicAtlasManager {
    /// width -> (height -> [rect])
    available_regions: Vec<(u32, Vec<(u32, AtlasRect)>)>,
}
impl DynamicAtlasManager {
    pub fn new() -> Self {
        Self {
            available_regions: Vec::new(),
        }
    }

    #[tracing::instrument(name = "DynamicAtlasManager::alloc", skip(self))]
    pub fn alloc(&mut self, width: u32, height: u32) -> Option<AtlasRect> {
        // find best match
        let mut match_index_by_width = match self
            .available_regions
            .binary_search_by_key(&width, |&(w, _)| w)
        {
            Ok(exact) => exact,
            Err(large_enough) if large_enough >= self.available_regions.len() => {
                tracing::warn!(
                    phase = "matching width",
                    available = ?self.available_regions.iter().map(|&(x, _)| x).collect::<Vec<_>>(),
                    "not found"
                );
                return None;
            }
            Err(large_enough) => large_enough,
        };
        let match_index_by_height = 'height_match_loop: loop {
            match self.available_regions[match_index_by_width]
                .1
                .binary_search_by_key(&height, |&(h, _)| h)
            {
                Ok(exact) => break 'height_match_loop exact,
                Err(large_enough)
                    if large_enough >= self.available_regions[match_index_by_width].1.len() =>
                {
                    tracing::warn!(
                        phase = "matching height",
                        available = ?self.available_regions[match_index_by_width].1.iter().map(|&(x, _)| x).collect::<Vec<_>>(),
                        "not found, trying more wider"
                    );
                    match_index_by_width += 1;
                    if match_index_by_width >= self.available_regions.len() {
                        tracing::error!("no available region found");
                        return None;
                    }
                }
                Err(large_enough) => break 'height_match_loop large_enough,
            }
        };

        let match_rects_by_width = &mut self.available_regions[match_index_by_width].1;
        let (_, mut rect) = match_rects_by_width.remove(match_index_by_height);
        if match_rects_by_width.is_empty() {
            // remove width line
            self.available_regions.remove(match_index_by_width);
        }

        match (rect.width() == width, rect.height() == height) {
            (true, true) => {
                // exact match
                return Some(rect);
            }
            (true, false) => {
                // split horizontally
                self.insert_rect_simple(rect.hsplit(height));
                Some(rect)
            }
            (false, true) => {
                // split vertically
                self.insert_rect_simple(rect.vsplit(width));
                Some(rect)
            }
            (false, false) => {
                // shrink
                self.insert_rect_simple(AtlasRect {
                    left: rect.left,
                    right: rect.right,
                    top: rect.top + height,
                    bottom: rect.bottom,
                });
                self.insert_rect_simple(AtlasRect {
                    left: rect.left + width,
                    right: rect.right,
                    top: rect.top,
                    bottom: rect.bottom,
                });
                rect.right = rect.left + width;
                rect.bottom = rect.top + height;
                Some(rect)
            }
        }
    }

    pub fn free(&mut self, rect: AtlasRect) {
        let inserted_indices = self.insert_rect_simple(rect);
        self.merge_recursively(inserted_indices);
    }

    #[inline(always)]
    fn rect(&self, nw: usize, nh: usize) -> &AtlasRect {
        &self.available_regions[nw].1[nh].1
    }

    fn merge_recursively(&mut self, pointing_rect_indices: (usize, usize)) {
        pub enum MergeOperation {
            ToLeft(usize, usize),
            ToRight(usize, usize),
            ToTop(usize, usize),
            ToBottom(usize, usize),
        }

        let merge_op = 'find_contact: {
            let rect =
                &self.available_regions[pointing_rect_indices.0].1[pointing_rect_indices.1].1;
            for (nw, (_, xs)) in self.available_regions.iter().enumerate() {
                for (nh, (_, r)) in xs.iter().enumerate() {
                    if r.bottom < rect.top
                        || r.right < rect.left
                        || r.top > rect.bottom
                        || r.left > rect.right
                    {
                        // never contacts
                        continue;
                    }

                    if rect.left == r.right && rect.top >= r.top && rect.bottom <= r.bottom {
                        break 'find_contact MergeOperation::ToLeft(nw, nh);
                    }
                    if rect.right == r.left && rect.top >= r.top && rect.bottom <= r.bottom {
                        break 'find_contact MergeOperation::ToRight(nw, nh);
                    }
                    if rect.top == r.bottom && rect.left >= r.left && rect.right <= r.right {
                        break 'find_contact MergeOperation::ToTop(nw, nh);
                    }
                    if rect.bottom == r.top && rect.left >= r.left && rect.right <= r.right {
                        break 'find_contact MergeOperation::ToBottom(nw, nh);
                    }
                }
            }

            // no contact: merging does nothing
            return;
        };

        match merge_op {
            MergeOperation::ToLeft(mut target_nw, mut target_nh) => {
                let target_rect = *self.rect(target_nw, target_nh);
                let (rect, shift_nw) =
                    self.unregister_rect_by_index(pointing_rect_indices.0, pointing_rect_indices.1);

                // adjust target indices
                if target_nw == pointing_rect_indices.0 && target_nh > pointing_rect_indices.1 {
                    target_nh -= 1;
                }
                if shift_nw && target_nw > pointing_rect_indices.0 {
                    target_nw -= 1;
                }

                let merged_rect = AtlasRect {
                    left: target_rect.left,
                    ..rect
                };

                if rect.top == target_rect.top && rect.bottom == target_rect.bottom {
                    // completely overlapped
                    self.unregister_rect_by_index(target_nw, target_nh);
                }
                let inserted_index = self.insert_rect_simple(merged_rect);
                self.merge_recursively(inserted_index);
            }
            MergeOperation::ToRight(mut target_nw, mut target_nh) => {
                let target_rect = *self.rect(target_nw, target_nh);
                let (rect, shift_nw) =
                    self.unregister_rect_by_index(pointing_rect_indices.0, pointing_rect_indices.1);

                // adjust target indices
                if target_nw == pointing_rect_indices.0 && target_nh > pointing_rect_indices.1 {
                    target_nh -= 1;
                }
                if shift_nw && target_nw > pointing_rect_indices.0 {
                    target_nw -= 1;
                }

                let merged_rect = AtlasRect {
                    right: target_rect.right,
                    ..rect
                };

                if rect.top == target_rect.top && rect.bottom == target_rect.bottom {
                    // completely overlapped
                    self.unregister_rect_by_index(target_nw, target_nh);
                }
                let inserted_index = self.insert_rect_simple(merged_rect);
                self.merge_recursively(inserted_index);
            }
            MergeOperation::ToTop(mut target_nw, mut target_nh) => {
                let target_rect = *self.rect(target_nw, target_nh);
                let (rect, shift_nw) =
                    self.unregister_rect_by_index(pointing_rect_indices.0, pointing_rect_indices.1);

                // adjust target indices
                if target_nw == pointing_rect_indices.0 && target_nh > pointing_rect_indices.1 {
                    target_nh -= 1;
                }
                if shift_nw && target_nw > pointing_rect_indices.0 {
                    target_nw -= 1;
                }

                let merged_rect = AtlasRect {
                    top: target_rect.top,
                    ..rect
                };

                if rect.left == target_rect.left && rect.right == target_rect.right {
                    // completely overlapped
                    self.unregister_rect_by_index(target_nw, target_nh);
                }
                let inserted_index = self.insert_rect_simple(merged_rect);
                self.merge_recursively(inserted_index);
            }
            MergeOperation::ToBottom(mut target_nw, mut target_nh) => {
                let target_rect = *self.rect(target_nw, target_nh);
                let (rect, shift_nw) =
                    self.unregister_rect_by_index(pointing_rect_indices.0, pointing_rect_indices.1);

                // adjust target indices
                if target_nw == pointing_rect_indices.0 && target_nh > pointing_rect_indices.1 {
                    target_nh -= 1;
                }
                if shift_nw && target_nw > pointing_rect_indices.0 {
                    target_nw -= 1;
                }

                let merged_rect = AtlasRect {
                    bottom: target_rect.bottom,
                    ..rect
                };

                if rect.left == target_rect.left && rect.right == target_rect.right {
                    // completely overlapped
                    self.unregister_rect_by_index(target_nw, target_nh);
                }
                let inserted_index = self.insert_rect_simple(merged_rect);
                self.merge_recursively(inserted_index);
            }
        }
    }

    fn unregister_rect_by_index(&mut self, nw: usize, nh: usize) -> (AtlasRect, bool) {
        let width_line = &mut self.available_regions[nw].1;
        let r = width_line.remove(nh).1;
        let width_line_removed = width_line.is_empty();
        if width_line_removed {
            self.available_regions.remove(nw);
        }

        (r, width_line_removed)
    }

    /// simply insert a rect into correct position(no merging performed)
    fn insert_rect_simple(&mut self, rect: AtlasRect) -> (usize, usize) {
        match self
            .available_regions
            .binary_search_by_key(&rect.width(), |&(w, _)| w)
        {
            Ok(width_index) => {
                let width_line = &mut self.available_regions[width_index].1;
                let height_insert_point = width_line
                    .binary_search_by_key(&rect.height(), |&(h, _)| h)
                    .map_or_else(|x| x, |x| x);
                width_line.insert(height_insert_point, (rect.height(), rect));

                (width_index, height_insert_point)
            }
            Err(insert_point) => {
                // unique for this width
                self.available_regions
                    .insert(insert_point, (rect.width(), vec![(rect.height(), rect)]));

                (insert_point, 0)
            }
        }
    }
}
