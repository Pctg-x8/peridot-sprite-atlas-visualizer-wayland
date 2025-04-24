use crate::{freetype as ft, harfbuzz as hb};
use bedrock::{self as br, DeviceMemoryMut, Image, MemoryBound, VkHandle};
use freetype2::*;

struct GlyphBitmap {
    pub buf: Box<[u8]>,
    pub width: usize,
    pub pitch: usize,
    pub rows: usize,
    pub left_offset: isize,
    pub ascending_pixels: isize,
}
impl GlyphBitmap {
    pub fn copy_from_ft_glyph_slot(slot: &FT_GlyphSlotRec) -> Self {
        assert!(
            slot.bitmap.pitch >= 0,
            "inverted flow is not supported at this point"
        );
        let bytes = slot.bitmap.pitch as usize * slot.bitmap.rows as usize;
        let mut buf = Vec::with_capacity(bytes);
        unsafe {
            buf.set_len(bytes);
        }
        let mut buf = buf.into_boxed_slice();
        unsafe {
            core::ptr::copy_nonoverlapping(slot.bitmap.buffer, buf.as_mut_ptr(), bytes);
        }

        Self {
            buf,
            width: slot.bitmap.width as _,
            pitch: slot.bitmap.pitch as _,
            rows: slot.bitmap.rows as _,
            left_offset: slot.bitmap_left as _,
            ascending_pixels: slot.bitmap_top as _,
        }
    }
}

pub struct TextLayout {
    bitmaps: Vec<GlyphBitmap>,
    final_left_pos: f32,
    final_top_pos: f32,
    max_ascender: i32,
    max_descender: i32,
}
impl TextLayout {
    pub fn build_simple(text: &str, face: &mut ft::Face) -> Self {
        let mut hb_buffer = hb::Buffer::new();
        hb_buffer.add(text);
        hb_buffer.guess_segment_properties();
        let mut hb_font = hb::Font::from_ft_face_referenced(face);
        hb::shape(&mut hb_font, &mut hb_buffer, &[]);
        let (glyph_infos, glyph_positions) = hb_buffer.get_shape_results();
        let mut left_pos = 0.0;
        let mut top_pos = 0.0;
        let mut max_ascender = 0;
        let mut max_descender = 0;
        // println!(
        //     "base metrics: {} {}",
        //     face.ascender_pixels(),
        //     face.height_pixels()
        // );
        let mut glyph_bitmaps = Vec::with_capacity(glyph_infos.len());
        for (info, pos) in glyph_infos.iter().zip(glyph_positions.iter()) {
            face.set_transform(
                None,
                Some(&FT_Vector {
                    x: (left_pos * 64.0) as _,
                    y: (top_pos * 64.0) as _,
                }),
            );
            face.load_glyph(info.codepoint, FT_LOAD_DEFAULT).unwrap();
            face.render_glyph(FT_RENDER_MODE_NORMAL).unwrap();
            let slot = face.glyph_slot().unwrap();

            // println!(
            //     "glyph {} {} {} {} {} {} {} {} {} {}",
            //     info.codepoint,
            //     pos.x_advance as f32 / 64.0,
            //     pos.y_advance as f32 / 64.0,
            //     pos.x_offset,
            //     pos.y_offset,
            //     slot.bitmap_left,
            //     slot.bitmap_top,
            //     slot.bitmap.width,
            //     slot.bitmap.rows,
            //     slot.bitmap.pitch,
            // );

            glyph_bitmaps.push(GlyphBitmap::copy_from_ft_glyph_slot(slot));

            left_pos += pos.x_advance as f32 / 64.0;
            top_pos += pos.y_advance as f32 / 64.0;
            max_ascender = max_ascender.max(slot.bitmap_top);
            max_descender = max_descender.max(slot.bitmap.rows as i32 - slot.bitmap_top);
        }
        // println!("final metrics: {left_pos} {top_pos} {max_ascender} {max_descender}");

        Self {
            bitmaps: glyph_bitmaps,
            final_left_pos: left_pos,
            final_top_pos: top_pos,
            max_ascender,
            max_descender,
        }
    }

    pub const fn width(&self) -> f32 {
        self.final_left_pos
    }

    #[inline]
    pub fn width_px(&self) -> u32 {
        self.width().ceil() as _
    }

    pub const fn height(&self) -> f32 {
        self.max_ascender as f32 + self.max_descender as f32
    }

    #[inline]
    pub fn height_px(&self) -> u32 {
        self.height().ceil() as _
    }

    pub fn build_stg_image<'d, D: br::Device + 'd>(
        &self,
        device: &'d D,
        adapter_memory_info: &br::MemoryProperties,
    ) -> (br::ImageObject<&'d D>, br::DeviceMemoryObject<&'d D>) {
        let mut img = br::ImageObject::new(
            device,
            &br::ImageCreateInfo::new(
                br::Extent2D {
                    width: self.width_px(),
                    height: self.height_px(),
                },
                br::vk::VK_FORMAT_R8_UNORM,
            )
            .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
            .use_linear_tiling(),
        )
        .expect("Failed to create staging text image");
        let mreq = img.requirements();
        let memory_index = adapter_memory_info
            .find_host_visible_index(mreq.memoryTypeBits)
            .expect("no suitable memory for image staging");
        let mut mem = br::DeviceMemoryObject::new(
            device,
            &br::MemoryAllocateInfo::new(mreq.size, memory_index),
        )
        .expect("Failed to allocate text surface stg memory");
        img.bind(&mem, 0).expect("Failed to bind stg memory");
        let subresource_layout =
            img.layout_info(&br::ImageSubresource::new(br::AspectMask::COLOR, 0, 0));

        let n = mem.native_ptr();
        let ptr = mem
            .map(0..(subresource_layout.rowPitch * self.height_px() as br::DeviceSize) as _)
            .unwrap();
        for b in self.bitmaps.iter() {
            for y in 0..b.rows {
                let dy = y as isize + self.max_ascender as isize - b.ascending_pixels;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        b.buf.as_ptr().add(b.pitch * y),
                        ptr.addr_of_mut(
                            subresource_layout.rowPitch as usize * dy as usize
                                + b.left_offset as usize,
                        ),
                        b.width,
                    )
                }
            }
        }
        if !adapter_memory_info.is_coherent(memory_index) {
            unsafe {
                device
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                        n,
                        0,
                        subresource_layout.rowPitch * self.height_px() as br::DeviceSize,
                    )])
                    .unwrap();
            }
        }
        ptr.end();

        (img, mem)
    }
}
