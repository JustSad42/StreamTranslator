//! Windows OCR (Windows.Media.Ocr), always called from one dedicated MTA thread: calling it from
//! arbitrary worker threads crashed the Python app once other native libraries were loaded.

use std::sync::{mpsc, Mutex, OnceLock};

use anyhow::{Context, Result};

pub struct Ocr {
    tx: mpsc::Sender<(Vec<u8>, mpsc::Sender<Result<Vec<String>>>)>,
}

impl Ocr {
    fn start() -> Ocr {
        let (tx, rx) = mpsc::channel::<(Vec<u8>, mpsc::Sender<Result<Vec<String>>>)>();
        std::thread::Builder::new()
            .name("windows-ocr".into())
            .spawn(move || {
                use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
                unsafe { RoInitialize(RO_INIT_MULTITHREADED).ok() };
                let engine = ocr_engine();
                for (png, reply) in rx {
                    let r = match &engine {
                        Ok(e) => ocr_png(e, &png),
                        Err(err) => Err(anyhow::anyhow!("{err}")),
                    };
                    let _ = reply.send(r);
                }
            })
            .unwrap();
        Ocr { tx }
    }
    pub fn read(&self, png: &[u8]) -> Result<Vec<String>> {
        let (rtx, rrx) = mpsc::channel();
        self.tx.send((png.to_vec(), rtx))?;
        rrx.recv()?
    }
}

fn ocr_engine() -> Result<windows::Media::Ocr::OcrEngine> {
    use windows::{core::HSTRING, Globalization::Language, Media::Ocr::OcrEngine};
    let lang = Language::CreateLanguage(&HSTRING::from("en-US"))?;
    OcrEngine::TryCreateFromLanguage(&lang)
        .or_else(|_| OcrEngine::TryCreateFromUserProfileLanguages())
        .context("Windows OCR isn't available")
}

fn ocr_png(engine: &windows::Media::Ocr::OcrEngine, png: &[u8]) -> Result<Vec<String>> {
    use windows::{Graphics::Imaging::BitmapDecoder, Storage::Streams::{DataWriter, InMemoryRandomAccessStream}};
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(png)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?.join()?;
    let bmp = decoder.GetSoftwareBitmapAsync()?.join()?;
    let res = engine.RecognizeAsync(&bmp)?.join()?;
    Ok(res.Lines()?.into_iter().map(|l| l.Text().map(|t| t.to_string()).unwrap_or_default()).collect())
}


static OCR: OnceLock<Mutex<Ocr>> = OnceLock::new();

/// Text lines in a PNG, read on the OCR thread (one image at a time).
pub fn ocr(png: &[u8]) -> Result<Vec<String>> {
    OCR.get_or_init(|| Mutex::new(Ocr::start())).lock().unwrap().read(png)
}
