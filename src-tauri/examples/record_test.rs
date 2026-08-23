// Screen-recording smoke test: records the display under the cursor for a few
// seconds through the real Swift FFI, then verifies the MP4 with ffprobe.
//
// Run from a terminal that HAS Screen Recording permission (TCC attributes a bare
// cargo binary to its terminal app):
//   cargo run --example record_test            # 3 s fullscreen
//   cargo run --example record_test -- --pause # 2 s rec / 2 s pause / 2 s rec (expects ~4 s)
//   cargo run --example record_test -- --audio # with a system-audio AAC track

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("record_test is macOS-only (the recorder is native there).");
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopRun();
}

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Duration;
    use teil_ing_lib::capture_macos as rec;
    use teil_ing_lib::recording::{BeginOutcome, RecordMode, RecordOpts, RecordState};

    let args: Vec<String> = std::env::args().collect();
    let with_pause = args.iter().any(|a| a == "--pause");
    let with_audio = args.iter().any(|a| a == "--audio");

    let out_path = std::env::temp_dir().join("teil-record-test.mp4");
    let _ = std::fs::remove_file(&out_path);
    let out = out_path.clone();

    // The recorder's MainActor hops (NSScreen/mouse lookups, frame window) are
    // serviced by the main run loop — so the test itself runs on a worker thread.
    std::thread::spawn(move || {
        let opts = RecordOpts {
            fps: 30,
            capture_audio: with_audio,
            show_cursor: true,
            max_bytes: 0,
            out_path: out.clone(),
        };
        match rec::record_begin(RecordMode::Fullscreen, &opts) {
            Ok(BeginOutcome::Started { width, height }) => {
                println!("recording started: {width}x{height} -> {}", out.display());
            }
            Ok(_) => {
                eprintln!("recording did not start (cancelled/busy)");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("record_begin failed: {e}");
                eprintln!("(missing Screen Recording permission for this terminal?)");
                std::process::exit(1);
            }
        }

        let expected_secs: f64;
        if with_pause {
            std::thread::sleep(Duration::from_secs(2));
            rec::record_pause().expect("pause");
            println!("paused: {:?}", status_line());
            std::thread::sleep(Duration::from_secs(2));
            rec::record_resume().expect("resume");
            println!("resumed");
            std::thread::sleep(Duration::from_secs(2));
            expected_secs = 4.0;
        } else {
            for _ in 0..6 {
                std::thread::sleep(Duration::from_millis(500));
                println!("status: {}", status_line());
            }
            expected_secs = 3.0;
        }

        let result = match rec::record_stop() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("record_stop failed: {e}");
                std::process::exit(1);
            }
        };
        println!(
            "stopped: {} ms, {} bytes, {}x{}, reason {:?}",
            result.duration_ms, result.bytes, result.width, result.height, result.reason
        );
        assert_eq!(rec::record_status().state, RecordState::Idle, "status must be idle after stop");

        verify(&out, expected_secs, with_audio, result.width, result.height);
        std::process::exit(0);
    });

    unsafe { CFRunLoopRun() };
}

#[cfg(target_os = "macos")]
fn status_line() -> String {
    let s = teil_ing_lib::capture_macos::record_status();
    format!(
        "{:?} {:?} {} ms {} bytes",
        s.state, s.reason, s.elapsed_ms, s.bytes
    )
}

#[cfg(target_os = "macos")]
fn verify(path: &std::path::Path, expected_secs: f64, with_audio: bool, width: u32, height: u32) {
    // ftyp magic at offset 4.
    let bytes = std::fs::read(path).expect("read output file");
    assert!(bytes.len() > 16, "file too small");
    assert_eq!(&bytes[4..8], b"ftyp", "missing ftyp box");
    let brand = String::from_utf8_lossy(&bytes[8..12]).to_string();
    println!("major brand: {brand}");
    assert!(
        ["mp42", "isom", "mp41", "iso5"].contains(&brand.as_str()),
        "unexpected major brand {brand}"
    );
    assert_eq!(width % 2, 0, "odd width");
    assert_eq!(height % 2, 0, "odd height");

    // ffprobe (best-effort — skip the deep checks if it's not installed).
    let ffprobe = ["/opt/homebrew/bin/ffprobe", "/usr/local/bin/ffprobe", "ffprobe"]
        .iter()
        .find(|p| std::process::Command::new(p).arg("-version").output().is_ok());
    let Some(ffprobe) = ffprobe else {
        println!("ffprobe not found — skipped codec/duration checks");
        return;
    };
    let out = std::process::Command::new(ffprobe)
        .args([
            "-v", "error",
            "-show_entries", "format=duration:stream=codec_type,codec_name,width,height",
            "-of", "json",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse ffprobe json");
    println!("ffprobe: {json}");

    let streams = json["streams"].as_array().expect("streams");
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .expect("no video stream");
    assert_eq!(video["codec_name"], "h264", "video codec");
    assert_eq!(video["width"].as_u64(), Some(width as u64));
    assert_eq!(video["height"].as_u64(), Some(height as u64));

    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    assert_eq!(has_audio, with_audio, "audio track presence");
    if with_audio {
        let audio = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
        assert_eq!(audio["codec_name"], "aac", "audio codec");
    }

    let duration: f64 = json["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse().ok())
        .expect("duration");
    println!("duration: {duration:.2}s (expected ~{expected_secs:.1}s)");
    assert!(
        (duration - expected_secs).abs() <= 0.5,
        "duration {duration:.2}s not within 0.5s of {expected_secs}"
    );
    println!("record_test OK");
}
