//! Text translation on the GPU with Meta's NLLB-200 (translate.py). Two sizes: "standard" (600M, ~1.2 GB)
//! and "better" (1.3B, ~2.7 GB), downloaded once into the shared Hugging Face cache.

use std::path::PathBuf;

use anyhow::Result;
use ct2rs_sys::{BatchType, ComputeType, Config, Device, TranslationOptions, Translator as Ct2Translator};
use st_core::{cancel::Cancel, langs::nllb_code, paths, subs::split_sentences};

use crate::gpu;

pub use st_core::models::{nllb_downloaded as is_downloaded, nllb_repo as repo};

pub struct Translator {
    model: Ct2Translator,
    sp: sentencepiece::SentencePieceProcessor,
    /// "standard" or "better"
    pub size: String,
    /// "GPU" or "CPU"
    pub device: &'static str,
}

unsafe impl Send for Translator {}
unsafe impl Sync for Translator {}

impl Translator {
    pub fn load(size: &str, status: &dyn Fn(&str), progress: &dyn Fn(f64), cancel: &Cancel) -> Result<Translator> {
        let size = if size == "standard" { "standard" } else { "better" };
        let (repo, gb) = repo(size);
        let dir: PathBuf = match paths::hf_snapshot(repo) {
            Some(d) => d,
            None => {
                status(&format!("Downloading the {size} translator (about {gb} GB, one time)…"));
                st_media::hub::snapshot(repo, st_core::models::NLLB_FILES, progress, cancel)?
            }
        };
        let gpu_ok = gpu::cuda_ready();
        let mut cfg = Config::default();
        if gpu_ok {
            cfg.device = Device::CUDA;
            cfg.compute_type = ComputeType::FLOAT16;
            cfg.device_indices = vec![0];
        } else {
            cfg.device = Device::CPU;
            cfg.compute_type = ComputeType::INT8;
        }
        let model = Ct2Translator::new(&dir, &cfg)?;
        let sp = sentencepiece::SentencePieceProcessor::open(dir.join("sentencepiece.bpe.model"))?;
        Ok(Translator { model, sp, size: size.into(), device: if gpu_ok { "GPU" } else { "CPU" } })
    }

    /// One call; CTranslate2 sorts by length and batches by token count (6x faster than fixed batches).
    pub fn batch(&self, pieces: &[String], code: &str, beam: usize) -> Result<Vec<String>> {
        if pieces.is_empty() {
            return Ok(vec![]);
        }
        let src: Vec<Vec<String>> = pieces
            .iter()
            .map(|t| {
                let mut v = vec![code.to_string()];
                v.extend(self.sp.encode(t).unwrap_or_default().into_iter().take(400).map(|p| p.piece));
                v.push("</s>".into());
                v
            })
            .collect();
        let prefix = vec![vec!["eng_Latn".to_string()]; src.len()];
        let mut o: TranslationOptions<String, String> = TranslationOptions::default();
        o.beam_size = beam;
        o.max_decoding_length = 256;
        o.repetition_penalty = 1.1;
        o.max_batch_size = 4096;
        o.batch_type = BatchType::Tokens;
        let res = self.model.translate_batch_with_target_prefix(&src, &prefix, &o, None)?;
        Ok(res.into_iter().map(|r| self.sp.decode_pieces(&r.hypotheses[0][1..]).unwrap_or_default()).collect())
    }

    /// Translate each text to English. Multi-sentence texts are split so nothing gets dropped.
    pub fn translate(&self, texts: &[String], src: &str, beam: usize) -> Result<Vec<String>> {
        let Some(code) = nllb_code(src) else { return Ok(texts.to_vec()) };
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let (mut owners, mut pieces) = (vec![], vec![]);
        for (i, t) in texts.iter().enumerate() {
            for s in split_sentences(t) {
                owners.push(i);
                pieces.push(s);
            }
        }
        let done = self.batch(&pieces, code, beam)?;
        let mut joined: Vec<Vec<String>> = vec![vec![]; texts.len()];
        for (i, s) in owners.into_iter().zip(done) {
            joined[i].push(s.trim().to_string());
        }
        Ok(joined.into_iter().map(|p| p.join(" ").trim().to_string()).collect())
    }
}
