//! Peridot Asset Format Definition

use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

use uuid::Uuid;

pub struct Sprite {
    pub id: Uuid,
    pub name: String,
    pub source_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub left: u32,
    pub top: u32,
    pub border_left: u32,
    pub border_top: u32,
    pub border_right: u32,
    pub border_bottom: u32,
}

pub struct SpriteAtlasAsset {
    /// needs sorted by id
    pub sprites: Vec<Sprite>,
    pub width: u32,
    pub height: u32,
}
impl SpriteAtlasAsset {
    pub fn write(&self, sink: &mut (impl Write + ?Sized)) -> std::io::Result<()> {
        writeln!(sink, "cfg={},{}", self.width, self.height)?;

        for &Sprite {
            ref id,
            ref name,
            ref source_path,
            width,
            height,
            left,
            top,
            border_left,
            border_top,
            border_right,
            border_bottom,
        } in self.sprites.iter()
        {
            // Note: 比較的変わりにくいもの -> 変わりやすいもの の順でならべている（行ごとの差分を見やすくするため）
            writeln!(
                sink,
                "{id}={width},{height},{border_left},{border_top},{border_right},{border_bottom},{left},{top},{source_path},{name}",
                id = id.as_simple(),
                source_path = source_path.display()
            )?;
        }

        Ok(())
    }

    pub fn read(src: &mut (impl BufRead + ?Sized)) -> Result<Self, SpriteAtlasAssetReadError> {
        let mut sprites = Vec::new();
        let mut width = 32;
        let mut height = 32;

        for l in src.lines() {
            let l = l?;
            let mut spl = l.splitn(2, '=');
            let id = spl.next().unwrap();
            let params = spl
                .next()
                .ok_or(SpriteAtlasAssetReadError::MissingSpriteParams)?;
            let mut params = params.split(',');

            if id == "cfg" {
                width = params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("width"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("width", e))?;
                height = params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("height"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("height", e))?;

                continue;
            }

            sprites.push(Sprite {
                id: id
                    .parse::<uuid::fmt::Simple>()
                    .map_err(SpriteAtlasAssetReadError::InvalidID)?
                    .into(),
                width: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("width"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("width", e))?,
                height: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("height"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("height", e))?,
                border_left: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("border_left"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("border_left", e))?,
                border_top: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("border_top"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("border_top", e))?,
                border_right: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("border_right"))?
                    .parse()
                    .map_err(|e| {
                        SpriteAtlasAssetReadError::InvalidParamFormat("border_right", e)
                    })?,
                border_bottom: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("border_bottom"))?
                    .parse()
                    .map_err(|e| {
                        SpriteAtlasAssetReadError::InvalidParamFormat("border_bottom", e)
                    })?,
                left: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("left"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("left", e))?,
                top: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("top"))?
                    .parse()
                    .map_err(|e| SpriteAtlasAssetReadError::InvalidParamFormat("top", e))?,
                source_path: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("source_path"))?
                    .into(),
                name: params
                    .next()
                    .ok_or(SpriteAtlasAssetReadError::MissingParam("name"))?
                    .into(),
            });
        }

        Ok(Self {
            sprites,
            width,
            height,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpriteAtlasAssetReadError {
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error("invalid id: {0}")]
    InvalidID(uuid::Error),
    #[error("missing sprite params")]
    MissingSpriteParams,
    #[error("missing {0}")]
    MissingParam(&'static str),
    #[error("invalid param format({0}): {1}")]
    InvalidParamFormat(&'static str, std::num::ParseIntError),
}
