use std::io::Read;

use shared_perflog_proto::{
    ProfileMarker, ProfileMarkerCategory, read_sample_head, validate_file_head,
};

fn main() {
    let mut fp = std::fs::File::open(std::env::args().nth(1).expect("file path missing")).unwrap();
    let Some((native_endian, ts_freq)) = validate_file_head(&mut fp).unwrap() else {
        panic!("invalid file header");
    };

    let mut last_framenumber = None;
    println!("frame,marker,category,timestamp");
    loop {
        let (marker, category, ts) = read_sample_head(&mut fp, !native_endian).unwrap();
        if marker == ProfileMarker::Frame && category == ProfileMarkerCategory::Begin {
            // one extra u32
            let mut x = 0u32;
            fp.read_exact(unsafe { core::mem::transmute::<_, &mut [u8; 4]>(&mut x) })
                .unwrap();
            if !native_endian {
                x = x.swap_bytes();
            }

            last_framenumber = Some(x);
        }

        println!(
            "{},{marker:?},{category:?},{}",
            last_framenumber.map_or_else(|| String::from("-1"), |x| x.to_string()),
            1000.0 * ts as f64 / ts_freq as f64
        );
    }
}
