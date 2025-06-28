use std::collections::HashSet;

pub struct QuadTreeElementIndexIter<'a> {
    qt: &'a QuadTree,
    index: u64,
    current_level: usize,
    current_internal_iter: Option<std::collections::hash_set::Iter<'a, usize>>,
}
impl Iterator for QuadTreeElementIndexIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        if let Some(a) = self.current_internal_iter.as_mut() {
            if let Some(&x) = a.next() {
                return Some(x);
            }

            // 全部回りきった
            self.current_internal_iter = None;
            self.current_level += 1;
        }

        // 下層の要素を探す
        while self.current_level < 32 {
            let index = if self.current_level == 0 {
                0
            } else {
                self.index >> (64 - self.current_level * 2)
            };
            let elements = &self
                .qt
                .element_index_for_region
                .get(self.current_level)
                .and_then(|xs| xs.get(index as usize));
            if let Some(elements) = elements.filter(|x| !x.is_empty()) {
                // この層には要素がある
                self.current_internal_iter = Some(elements.iter());
                return self.next();
            }

            self.current_level += 1;
        }

        // もうない
        None
    }
}

/// ビットを一つおきに分散させる
/// 例: 0b11000110 => 0b01_01_00_00_00_01_01_00
const fn interleave(bits: u64) -> u64 {
    let bits = (bits | (bits << 32)) & 0xffff_ffff_ffff_ffff;
    let bits = (bits | (bits << 16)) & 0x0000_ffff_0000_ffff;
    let bits = (bits | (bits << 8)) & 0x00ff_00ff_00ff_00ff;
    let bits = (bits | (bits << 4)) & 0x0f0f_0f0f_0f0f_0f0f;
    let bits = (bits | (bits << 2)) & 0x3333_3333_3333_3333;
    let bits = (bits | (bits << 1)) & 0x5555_5555_5555_5555;

    bits
}

// http://marupeke296.com/COL_2D_No8_QuadTree.html だいたいこれの実装
pub struct QuadTree {
    pub element_index_for_region: Vec<Vec<HashSet<usize>>>,
}
impl QuadTree {
    pub fn new() -> Self {
        Self {
            element_index_for_region: Vec::new(),
        }
    }

    pub fn bind(&mut self, level: usize, index: u64, n: usize) {
        while self.element_index_for_region.len() <= level {
            self.element_index_for_region.push(Vec::new());
        }

        while self.element_index_for_region[level].len() <= index as _ {
            self.element_index_for_region[level].push(HashSet::new());
        }

        self.element_index_for_region[level][index as usize].insert(n);
    }

    pub const fn iter_possible_element_indices(
        &self,
        x_pixels: u32,
        y_pixels: u32,
    ) -> impl Iterator<Item = usize> {
        QuadTreeElementIndexIter {
            qt: self,
            index: Self::compute_location_index(x_pixels, y_pixels),
            current_level: 0,
            current_internal_iter: None,
        }
    }

    pub const fn compute_location_index(location_x_pixels: u32, location_y_pixels: u32) -> u64 {
        // 一旦一律16(2^4)px角まで分割する
        let (xv, yv) = (
            (location_x_pixels >> 4) as u64,
            (location_y_pixels >> 4) as u64,
        );

        // のちのシフト操作で情報が欠けないように検査いれる
        assert!(xv.leading_zeros() >= 32, "too many divisions!");
        assert!(yv.leading_zeros() >= 32, "too many divisions!");

        interleave(xv) | (interleave(yv) << 1)
    }

    pub const fn rect_index_and_level(
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    ) -> (u64, usize) {
        let lt_location = Self::compute_location_index(left, top);
        let rb_location = Self::compute_location_index(right, bottom);
        // xorをとるとズレているレベルの2bitが00にならないので、それでどの分割レベルで跨いでいないか（どのレベルの所属インデックスまでが一致しているか）を判定できるっぽい
        let xor = lt_location ^ rb_location;

        // 先頭の0のビット数を数えて、
        // 0, 1...Lv0(root, 分割無し)
        // 2, 3...Lv1(全体を4分割したうちのどこか)
        // 4, 5...Lv2(全体を16分割したうちのどこか)
        // ...となるように計算する
        let level = (xor.leading_zeros() / 2) as usize;
        // 符号なし整数なので右シフト後は上が0で埋まるはず
        let index = lt_location >> (64 - level * 2);

        (index, level)
    }
}
