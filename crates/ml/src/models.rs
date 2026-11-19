//! The model files each AI feature needs, where they come from, and which are installed.
//! Files are pinned to a fixed revision of their Hugging Face repository and checked
//! against their SHA-256 after downloading (see [`crate::download`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::segment::{SUBJECT, SUBJECT_FAST};
use crate::{CLIP_TEXT, CLIP_TOKENIZER, CLIP_VISION, SFACE, YUNET};

/// Something the user can choose to download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feature {
    /// Finding faces and grouping them into people (YuNet + SFace).
    Faces,
    /// Searching by what is in a photo, and the editor's scene suggestions (CLIP).
    Search,
    /// The editor's background blur (BiRefNet).
    Blur,
}

impl Feature {
    pub const ALL: [Feature; 3] = [Feature::Faces, Feature::Search, Feature::Blur];

    /// The files to download. Blur comes in two sizes: `full` for PCs with a usable
    /// graphics card, the lighter one otherwise.
    pub fn files(self, full: bool) -> &'static [ModelFile] {
        match self {
            Feature::Faces => FACES,
            Feature::Search => SEARCH,
            Feature::Blur if full => BLUR_FULL,
            Feature::Blur => BLUR_FAST,
        }
    }

    /// The folder among `dirs` that has every file of the feature (either size for blur).
    pub fn installed_in(self, dirs: &[PathBuf]) -> Option<PathBuf> {
        match self {
            Feature::Blur => find(dirs, BLUR_FULL).or_else(|| find(dirs, BLUR_FAST)),
            f => find(dirs, f.files(true)),
        }
    }
}

/// One file of a model.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    /// Where it goes in a models folder.
    pub path: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// The first of `dirs` holding all of `files`.
pub fn find(dirs: &[PathBuf], files: &[ModelFile]) -> Option<PathBuf> {
    dirs.iter().find(|d| has(d, files)).cloned()
}

pub fn has(dir: &Path, files: &[ModelFile]) -> bool {
    files.iter().all(|f| dir.join(f.path).is_file())
}

pub fn total_size(files: &[ModelFile]) -> u64 {
    files.iter().map(|f| f.size).sum()
}

macro_rules! hf {
    ($repo:literal, $rev:literal, $file:literal) => {
        concat!("https://huggingface.co/", $repo, "/resolve/", $rev, "/", $file)
    };
}

pub const FACES: &[ModelFile] = &[
    ModelFile {
        path: YUNET,
        url: hf!("opencv/face_detection_yunet", "3cc26e7f1014a5ee5d74a42acee58bafc9d0a310", "face_detection_yunet_2023mar.onnx"),
        size: 232_589,
        sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    },
    ModelFile {
        path: SFACE,
        url: hf!("opencv/face_recognition_sface", "3d7082438a6e4551e840c9b2bb60b71e8da4b524", "face_recognition_sface_2021dec.onnx"),
        size: 38_696_353,
        sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
    },
];

pub const SEARCH: &[ModelFile] = &[
    ModelFile {
        path: CLIP_VISION,
        url: hf!("Xenova/clip-vit-base-patch32", "6b7c523703ae26e39be4b03774966f8571c7f523", "onnx/vision_model_quantized.onnx"),
        size: 89_117_001,
        sha256: "583fd1110a514667812fee7d684952aaf82a99b959760c8d7dca7e0ab9839299",
    },
    ModelFile {
        path: CLIP_TEXT,
        url: hf!("Xenova/clip-vit-base-patch32", "6b7c523703ae26e39be4b03774966f8571c7f523", "onnx/text_model_quantized.onnx"),
        size: 64_504_507,
        sha256: "73baab855d406190da9faa498cfedf65f15cf309f4cc7385b7b032e6d08e5c3a",
    },
    ModelFile {
        path: CLIP_TOKENIZER,
        url: hf!("Xenova/clip-vit-base-patch32", "6b7c523703ae26e39be4b03774966f8571c7f523", "tokenizer.json"),
        size: 2_224_119,
        sha256: "f7f3b7af117d467b58374797691a6438d3e6b9e9cef800dfd5dced7f697a90cd",
    },
];

pub const BLUR_FULL: &[ModelFile] = &[ModelFile {
    path: SUBJECT,
    url: hf!("onnx-community/BiRefNet_lite-ONNX", "de15b22ba131738a16dff04aab8bdf8dc32e3ac1", "onnx/model.onnx"),
    size: 224_005_088,
    sha256: "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333",
}];

pub const BLUR_FAST: &[ModelFile] = &[ModelFile {
    path: SUBJECT_FAST,
    url: hf!("studioludens/birefnet-lite-512", "962e67969ec6acec9c19fab76acee4f5e22e990c", "onnx/model.onnx"),
    size: 191_877_254,
    sha256: "1cb0fb360dadd15af77c639085d77a9df67db0c64315560c3de005f676345ac2",
}];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_file_belongs_to_a_feature() {
        let all: Vec<&str> = [FACES, SEARCH, BLUR_FULL, BLUR_FAST].iter().flat_map(|f| f.iter().map(|m| m.path)).collect();
        for f in crate::MODEL_FILES.iter().chain([&SUBJECT, &SUBJECT_FAST]) {
            assert!(all.contains(f), "{f}");
        }
        for m in [FACES, SEARCH, BLUR_FULL, BLUR_FAST].iter().flat_map(|f| f.iter()) {
            assert_eq!(m.sha256.len(), 64, "{}", m.path);
            assert!(m.url.starts_with("https://huggingface.co/") && !m.url.contains("/resolve/main/"), "{}", m.url);
        }
    }

    #[test]
    fn blur_counts_as_installed_in_either_size() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        assert!(Feature::Blur.installed_in(&dirs).is_none());
        let p = dir.path().join(SUBJECT_FAST);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
        assert_eq!(Feature::Blur.installed_in(&dirs).as_deref(), Some(dir.path()));
        assert!(Feature::Faces.installed_in(&dirs).is_none());
    }
}
