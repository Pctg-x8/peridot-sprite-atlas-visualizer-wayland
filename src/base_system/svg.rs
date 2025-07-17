use std::{path::Path, str::FromStr};

fn str_bytes_take_float_value(mut s: &[u8]) -> (f32, &[u8]) {
    let neg = if let &[b'-', ref rest @ ..] = s {
        s = rest;
        true
    } else {
        false
    };

    let starting = s;
    let mut byte_count = 0;
    while let &[c, ref rest @ ..] = s {
        if c != b'.' && !c.is_ascii_digit() {
            break;
        }

        s = rest;
        byte_count += 1;
    }

    let v = unsafe { core::str::from_utf8_unchecked(&starting[..byte_count]) }
        .parse::<f32>()
        .unwrap();
    (if neg { -v } else { v }, s)
}

#[derive(Debug, Clone)]
pub struct ViewBox {
    pub min_x: f32,
    pub min_y: f32,
    pub width: f32,
    pub height: f32,
}
impl ViewBox {
    pub fn from_str_bytes(s: &[u8]) -> Self {
        const fn process_part_splitter(s: &mut &[u8]) {
            while let &[c, ref rest @ ..] = *s {
                if !c.is_ascii_whitespace() && c != b',' {
                    break;
                }

                *s = rest;
            }
        }

        let s = s.trim_ascii_start();

        let (min_x, mut s) = str_bytes_take_float_value(s);
        process_part_splitter(&mut s);
        let (min_y, mut s) = str_bytes_take_float_value(s);
        process_part_splitter(&mut s);
        let (width, mut s) = str_bytes_take_float_value(s);
        process_part_splitter(&mut s);
        let (height, _) = str_bytes_take_float_value(s);

        Self {
            min_x,
            min_y,
            width,
            height,
        }
    }

    pub const fn translate_x(&self, x: f32) -> f32 {
        (x - self.min_x) / self.width
    }

    pub const fn translate_y(&self, y: f32) -> f32 {
        (y - self.min_y) / self.height
    }
}
impl FromStr for ViewBox {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_str_bytes(s.as_bytes()))
    }
}

#[derive(Debug, Clone)]
pub enum Instruction {
    Move {
        absolute: bool,
        x: f32,
        y: f32,
    },
    Line {
        absolute: bool,
        x: f32,
        y: f32,
    },
    HLine {
        absolute: bool,
        x: f32,
    },
    VLine {
        absolute: bool,
        y: f32,
    },
    BezierCurve {
        absolute: bool,
        c1_x: f32,
        c1_y: f32,
        c2_x: f32,
        c2_y: f32,
        x: f32,
        y: f32,
    },
    SequentialBezierCurve {
        absolute: bool,
        c2_x: f32,
        c2_y: f32,
        x: f32,
        y: f32,
    },
    QuadraticBezierCurve {
        absolute: bool,
        cx: f32,
        cy: f32,
        x: f32,
        y: f32,
    },
    SequentialQuadraticBezierCurve {
        absolute: bool,
        x: f32,
        y: f32,
    },
    Arc {
        absolute: bool,
        rx: f32,
        ry: f32,
        angle: f32,
        large_arc_flag: bool,
        sweep_flag: bool,
        x: f32,
        y: f32,
    },
    Close,
}

#[derive(Clone, Copy)]
enum InstType {
    Move { absolute: bool },
    Line { absolute: bool },
    HLine { absolute: bool },
    VLine { absolute: bool },
    BezierCurve { absolute: bool },
    SequentialBezierCurve { absolute: bool },
    QuadraticBezierCurve { absolute: bool },
    SequentialQuadraticBezierCurve { absolute: bool },
    Arc { absolute: bool },
    Close,
}
impl InstType {
    const fn from_byte(b: u8) -> Option<Self> {
        match b {
            b'M' => Some(Self::Move { absolute: true }),
            b'm' => Some(Self::Move { absolute: false }),
            b'L' => Some(Self::Line { absolute: true }),
            b'l' => Some(Self::Line { absolute: false }),
            b'H' => Some(Self::HLine { absolute: true }),
            b'h' => Some(Self::HLine { absolute: false }),
            b'V' => Some(Self::VLine { absolute: true }),
            b'v' => Some(Self::VLine { absolute: false }),
            b'C' => Some(Self::BezierCurve { absolute: true }),
            b'c' => Some(Self::BezierCurve { absolute: false }),
            b'S' => Some(Self::SequentialBezierCurve { absolute: true }),
            b's' => Some(Self::SequentialBezierCurve { absolute: false }),
            b'Q' => Some(Self::QuadraticBezierCurve { absolute: true }),
            b'q' => Some(Self::QuadraticBezierCurve { absolute: false }),
            b'T' => Some(Self::SequentialQuadraticBezierCurve { absolute: true }),
            b't' => Some(Self::SequentialQuadraticBezierCurve { absolute: false }),
            b'A' => Some(Self::Arc { absolute: true }),
            b'a' => Some(Self::Arc { absolute: false }),
            b'Z' | b'z' => Some(Self::Close),
            _ => None,
        }
    }
}

pub struct InstructionParser<'s> {
    source: &'s [u8],
    last_inst_type: Option<InstType>,
}
impl<'s> InstructionParser<'s> {
    pub const fn new_bytes(source: &'s [u8]) -> Self {
        Self {
            source,
            last_inst_type: None,
        }
    }

    const fn skip_spaces(&mut self) {
        while let [first, rest @ ..] = self.source {
            if !first.is_ascii_whitespace() {
                break;
            }

            self.source = rest;
        }
    }

    fn take_coord_value(&mut self) -> f32 {
        let (v, s) = str_bytes_take_float_value(self.source);

        self.source = s;
        v
    }

    const fn process_coord_strip(&mut self) {
        while let &[c, ref rest @ ..] = self.source {
            if c != b',' && !c.is_ascii_whitespace() {
                break;
            }

            self.source = rest;
        }
    }
}
impl<'s> Iterator for InstructionParser<'s> {
    type Item = Instruction;

    fn next(&mut self) -> Option<Self::Item> {
        self.skip_spaces();

        let &[c, ref rest @ ..] = self.source else {
            return None;
        };

        let inst = if let Some(inst) = InstType::from_byte(c) {
            self.source = rest;
            self.last_inst_type = Some(inst);
            inst
        } else {
            if matches!(self.last_inst_type, Some(InstType::Close)) {
                panic!("close command never be consequenced");
            }

            self.last_inst_type.unwrap()
        };

        match inst {
            InstType::Move { absolute } => {
                self.source = rest;
                self.skip_spaces();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::Move { absolute, x, y })
            }
            InstType::Line { absolute } => {
                self.skip_spaces();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::Line { absolute, x, y })
            }
            InstType::HLine { absolute } => {
                self.source = rest;
                self.skip_spaces();
                let x = self.take_coord_value();

                Some(Instruction::HLine { absolute, x })
            }
            InstType::VLine { absolute } => {
                self.source = rest;
                self.skip_spaces();
                let y = self.take_coord_value();

                Some(Instruction::VLine { absolute, y })
            }
            InstType::BezierCurve { absolute } => {
                self.skip_spaces();
                let c1_x = self.take_coord_value();
                self.process_coord_strip();
                let c1_y = self.take_coord_value();
                self.process_coord_strip();
                let c2_x = self.take_coord_value();
                self.process_coord_strip();
                let c2_y = self.take_coord_value();
                self.process_coord_strip();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::BezierCurve {
                    absolute,
                    c1_x,
                    c1_y,
                    c2_x,
                    c2_y,
                    x,
                    y,
                })
            }
            InstType::SequentialBezierCurve { absolute } => {
                self.source = rest;
                self.skip_spaces();
                let c2_x = self.take_coord_value();
                self.process_coord_strip();
                let c2_y = self.take_coord_value();
                self.process_coord_strip();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::SequentialBezierCurve {
                    absolute,
                    c2_x,
                    c2_y,
                    x,
                    y,
                })
            }
            InstType::QuadraticBezierCurve { absolute } => {
                self.skip_spaces();
                let cx = self.take_coord_value();
                self.process_coord_strip();
                let cy = self.take_coord_value();
                self.process_coord_strip();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::QuadraticBezierCurve {
                    absolute,
                    cx,
                    cy,
                    x,
                    y,
                })
            }
            InstType::SequentialQuadraticBezierCurve { absolute } => {
                self.skip_spaces();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::SequentialQuadraticBezierCurve { absolute, x, y })
            }
            InstType::Arc { absolute } => {
                self.skip_spaces();
                let rx = self.take_coord_value();
                self.process_coord_strip();
                let ry = self.take_coord_value();
                self.process_coord_strip();
                let angle = self.take_coord_value();
                self.process_coord_strip();
                let large_arc_flag = self.take_coord_value() as i32 == 1;
                self.process_coord_strip();
                let sweep_flag = self.take_coord_value() as i32 == 1;
                self.process_coord_strip();
                let x = self.take_coord_value();
                self.process_coord_strip();
                let y = self.take_coord_value();

                Some(Instruction::Arc {
                    absolute,
                    rx,
                    ry,
                    angle,
                    large_arc_flag,
                    sweep_flag,
                    x,
                    y,
                })
            }
            InstType::Close => Some(Instruction::Close),
        }
    }
}

pub struct SinglePathSVG {
    pub viewbox: ViewBox,
    pub instructions: Vec<Instruction>,
}
impl SinglePathSVG {
    #[tracing::instrument(name = "SinglePathSVG::load", fields(path = %path.as_ref().display()))]
    pub fn load(path: impl AsRef<Path>) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "reading svg content failed");
                std::process::abort();
            }
        };

        let mut reader = quick_xml::Reader::from_str(&content);
        let mut instructions = Vec::new();
        let mut viewbox = None;
        let svg_start = match read_for_svg_tag(&mut reader) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "reading svg failed");
                std::process::abort();
            }
        };
        for a in svg_start.attributes().with_checks(false) {
            let a = a.unwrap();

            if a.key.0 == b"viewBox" {
                viewbox = Some(ViewBox::from_str_bytes(&a.value));
            }
        }
        loop {
            match reader.read_event() {
                Err(e) => {
                    tracing::error!(reason = ?e, "reading svg failed");
                    std::process::abort();
                }
                Ok(quick_xml::events::Event::End(x)) if x.name().0 == b"svg" => {
                    // end of svg content
                    break;
                }
                Ok(quick_xml::events::Event::Empty(x)) if x.name().0 == b"path" => {
                    for a in x.attributes().with_checks(false) {
                        let a = a.unwrap();

                        if a.key.0 == b"d" {
                            for x in InstructionParser::new_bytes(&a.value) {
                                instructions.push(x);
                            }
                        }
                    }
                }
                Ok(e) => {
                    tracing::error!(event = ?e, "unexpected");
                    unreachable!();
                }
            }
        }
        match reader.read_event() {
            Err(e) => {
                tracing::error!(reason = ?e, "reading svg failed");
                std::process::abort();
            }
            Ok(quick_xml::events::Event::Eof) => (),
            Ok(e) => {
                tracing::error!(event = ?e, "unexpected");
                unreachable!();
            }
        }

        Self {
            viewbox: match viewbox {
                Some(x) => x,
                None => {
                    tracing::error!("no viewbox?");
                    unreachable!();
                }
            },
            instructions,
        }
    }
}

fn read_for_svg_tag<'a>(
    reader: &mut quick_xml::Reader<&'a [u8]>,
) -> Result<quick_xml::events::BytesStart<'a>, quick_xml::Error> {
    loop {
        match reader.read_event()? {
            quick_xml::events::Event::Start(x) if x.name().0 == b"svg" => return Ok(x),
            quick_xml::events::Event::Start(x) => {
                tracing::error!(tag = ?x, "unexpected start tag");
                unreachable!();
            }
            e => {
                tracing::error!(event = ?e, "unexpected");
                unreachable!();
            }
        }
    }
}
