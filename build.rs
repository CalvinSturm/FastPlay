use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

struct FfmpegPaths {
    include_dir: PathBuf,
    lib_dir: PathBuf,
    bin_dir: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/ffi/ffmpeg_shim.c");
    println!("cargo:rerun-if-changed=src/ffi/ffmpeg_shim.h");

    let ffmpeg = discover_ffmpeg()?;

    println!("cargo:rustc-link-search=native={}", ffmpeg.lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=avcodec");
    println!("cargo:rustc-link-lib=dylib=avformat");
    println!("cargo:rustc-link-lib=dylib=swscale");
    println!("cargo:rustc-link-lib=dylib=swresample");
    println!("cargo:rustc-link-lib=dylib=avutil");

    if let Some(bin_dir) = &ffmpeg.bin_dir {
        stage_ffmpeg_dlls(bin_dir)?;
    }

    compile_ffmpeg_shim(&ffmpeg.include_dir);
    generate_ffmpeg_bindings(&ffmpeg.include_dir)?;

    Ok(())
}

fn discover_ffmpeg() -> Result<FfmpegPaths, Box<dyn Error>> {
    if let Some(paths) = discover_from_explicit_env()? {
        return Ok(paths);
    }

    for root in candidate_roots() {
        if let Some(paths) = layout_from_root(&root) {
            return Ok(paths);
        }
    }

    Err("FFmpeg development files were not found. Set FFMPEG_DIR or FFMPEG_INCLUDE_DIR/FFMPEG_LIB_DIR[/FFMPEG_BIN_DIR], or set VCPKG_ROOT with an installed x64-windows ffmpeg package.".into())
}

fn discover_from_explicit_env() -> Result<Option<FfmpegPaths>, Box<dyn Error>> {
    let include_dir = env::var_os("FFMPEG_INCLUDE_DIR");
    let lib_dir = env::var_os("FFMPEG_LIB_DIR");
    let bin_dir = env::var_os("FFMPEG_BIN_DIR");

    match (include_dir, lib_dir) {
        (Some(include_dir), Some(lib_dir)) => {
            let include_dir = PathBuf::from(include_dir);
            let lib_dir = PathBuf::from(lib_dir);
            validate_include_and_lib(&include_dir, &lib_dir)?;
            Ok(Some(FfmpegPaths {
                include_dir,
                lib_dir,
                bin_dir: bin_dir.map(PathBuf::from).filter(|path| path.exists()),
            }))
        }
        (None, None) => {
            if let Some(root) = env::var_os("FFMPEG_DIR") {
                Ok(layout_from_root(&PathBuf::from(root)))
            } else {
                Ok(None)
            }
        }
        _ => Err("FFMPEG_INCLUDE_DIR and FFMPEG_LIB_DIR must be set together when using explicit FFmpeg paths.".into()),
    }
}

fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(vcpkg_root) = env::var_os("VCPKG_ROOT") {
        roots.push(PathBuf::from(vcpkg_root).join("installed").join("x64-windows"));
    }

    if let Some(user_profile) = env::var_os("USERPROFILE") {
        roots.push(
            PathBuf::from(user_profile)
                .join("vcpkg")
                .join("installed")
                .join("x64-windows"),
        );
    }

    roots.push(PathBuf::from(r"C:\tools\vcpkg\installed\x64-windows"));
    roots
}

fn layout_from_root(root: &Path) -> Option<FfmpegPaths> {
    let include_dir = root.join("include");
    let lib_dir = root.join("lib");
    let bin_dir = root.join("bin");

    if !has_ffmpeg_headers(&include_dir) || !has_ffmpeg_libs(&lib_dir) {
        return None;
    }

    Some(FfmpegPaths {
        include_dir,
        lib_dir,
        bin_dir: bin_dir.exists().then_some(bin_dir),
    })
}

fn validate_include_and_lib(include_dir: &Path, lib_dir: &Path) -> Result<(), Box<dyn Error>> {
    if !has_ffmpeg_headers(include_dir) {
        return Err(format!("FFmpeg headers were not found under {}", include_dir.display()).into());
    }

    if !has_ffmpeg_libs(lib_dir) {
        return Err(format!("FFmpeg import libraries were not found under {}", lib_dir.display()).into());
    }

    Ok(())
}

fn has_ffmpeg_headers(include_dir: &Path) -> bool {
    include_dir.join("libavcodec").join("avcodec.h").exists()
        && include_dir.join("libavformat").join("avformat.h").exists()
        && include_dir.join("libavutil").join("hwcontext_d3d11va.h").exists()
}

fn has_ffmpeg_libs(lib_dir: &Path) -> bool {
    lib_dir.join("avcodec.lib").exists()
        && lib_dir.join("avformat.lib").exists()
        && lib_dir.join("avutil.lib").exists()
}

fn stage_ffmpeg_dlls(bin_dir: &Path) -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let profile = env::var("PROFILE")?;
    let target_dir = manifest_dir.join("target").join(profile);

    for entry in fs::read_dir(bin_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("dll") {
            let dst = target_dir.join(entry.file_name());
            fs::copy(path, dst)?;
        }
    }

    Ok(())
}

fn compile_ffmpeg_shim(include_dir: &Path) {
    cc::Build::new()
        .file("src/ffi/ffmpeg_shim.c")
        .include(include_dir)
        .compile("fastplay_ffmpeg_shim");
}

fn generate_ffmpeg_bindings(include_dir: &Path) -> Result<(), Box<dyn Error>> {
    let out_path = PathBuf::from(env::var("OUT_DIR")?).join("ffmpeg_bindings.rs");

    if env::var_os("LIBCLANG_PATH").is_none() {
        let default_libclang = PathBuf::from(r"C:\Program Files\LLVM\bin");
        if default_libclang.exists() {
            env::set_var("LIBCLANG_PATH", default_libclang);
        }
    }

    let bindings = bindgen::Builder::default()
        .header("src/ffi/ffmpeg_shim.h")
        .clang_arg(format!("-I{}", include_dir.display()))
        .clang_arg("--target=x86_64-pc-windows-msvc")
        .allowlist_type("AV.*")
        .allowlist_type("AVD3D11VA.*")
        .allowlist_type("ID3D11.*")
        .allowlist_type("SwsContext")
        .allowlist_type("SwrContext")
        .allowlist_function("av_buffer_ref")
        .allowlist_function("av_buffer_unref")
        .allowlist_function("av_codec_is_decoder")
        .allowlist_function("av_find_best_stream")
        .allowlist_function("av_frame_alloc")
        .allowlist_function("av_frame_free")
        .allowlist_function("av_frame_unref")
        .allowlist_function("av_hwdevice_ctx_alloc")
        .allowlist_function("av_hwdevice_ctx_init")
        .allowlist_function("av_packet_alloc")
        .allowlist_function("av_packet_free")
        .allowlist_function("av_packet_unref")
        .allowlist_function("av_read_frame")
        .allowlist_function("av_strerror")
        .allowlist_function("avcodec_alloc_context3")
        .allowlist_function("avcodec_free_context")
        .allowlist_function("avcodec_get_hw_config")
        .allowlist_function("avcodec_open2")
        .allowlist_function("avcodec_parameters_to_context")
        .allowlist_function("avcodec_receive_frame")
        .allowlist_function("avcodec_send_packet")
        .allowlist_function("avformat_close_input")
        .allowlist_function("avformat_find_stream_info")
        .allowlist_function("avformat_open_input")
        .allowlist_function("sws_.*")
        .allowlist_function("swr_.*")
        .allowlist_function("fastplay_ffmpeg_.*")
        .allowlist_var("AV_CH_LAYOUT_.*")
        .allowlist_var("AVERROR_.*")
        .allowlist_var("AVMEDIA_TYPE_.*")
        .allowlist_var("AV_PIX_FMT_.*")
        .allowlist_var("AV_SAMPLE_FMT_.*")
        .allowlist_var("AV_HWDEVICE_TYPE_.*")
        .allowlist_var("AV_CODEC_HW_CONFIG_METHOD_.*")
        .allowlist_var("SWS_.*")
        .derive_debug(false)
        .layout_tests(false)
        .generate_comments(true)
        .generate()?;

    bindings.write_to_file(out_path)?;
    Ok(())
}
