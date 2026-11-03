use std::path::{Path, PathBuf};

fn main() {
    // Cargo.toml か build.rs が更新されたら、バインディングファイルを再生成する
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=build.rs");
    // docs.rs ビルドかどうかでバインディングの生成方法が変わるため、環境変数の変更を監視する
    println!("cargo::rerun-if-env-changed=DOCS_RS");

    // 実機 Intel GPU を使うテストを cfg(intel_vpl) でガードする
    // 環境変数 INTEL_VPL=1 が設定されている場合のみ有効になる
    println!("cargo::rustc-check-cfg=cfg(intel_vpl)");
    println!("cargo::rerun-if-env-changed=INTEL_VPL");
    if std::env::var("INTEL_VPL").as_deref() == Ok("1") {
        println!("cargo::rustc-cfg=intel_vpl");
    }

    // 各種変数やビルドディレクトリのセットアップ
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("infallible"));
    let output_bindings_path = out_dir.join("bindings.rs");
    let output_metadata_path = out_dir.join("metadata.rs");

    // Cargo.toml からメタデータを読み取る
    let (vpl_version, vpl_url) = get_metadata();

    // バージョンメタデータを OUT_DIR に書き込む
    std::fs::write(
        output_metadata_path,
        format!(
            "pub const BUILD_METADATA_VERSION: &str = {:?};\n",
            vpl_version
        ),
    )
    .expect("failed to write metadata file");

    // docs.rs のビルド環境はネットワークに接続できないため、libvpl の取得と
    // bindgen によるバインディング生成をスキップし、ドキュメント生成用の
    // ダミー定義を出力する
    if std::env::var("DOCS_RS").is_ok() {
        std::fs::write(&output_bindings_path, DOCS_RS_DUMMY_BINDINGS)
            .expect("failed to write dummy bindings");
        return;
    }

    // GitHub から libvpl ヘッダをクローンして include ディレクトリのパスを取得する
    let clone_dir = out_dir.join("libvpl");
    let vpl_api_dir = clone_vpl_headers(&clone_dir, &vpl_version, &vpl_url);
    let mfx_header = vpl_api_dir.join("vpl/mfx.h");

    if !mfx_header.exists() {
        panic!("mfx.h not found at {:?}", mfx_header);
    }

    // バインディングを生成する
    let bindings = bindgen::Builder::default()
        .header(mfx_header.display().to_string())
        .clang_arg(format!("-I{}", vpl_api_dir.display()))
        .generate_comments(false)
        .derive_debug(false)
        .derive_default(false)
        .parse_callbacks(Box::new(CustomCallbacks))
        .generate()
        .expect("failed to generate bindings");

    // OUT_DIR にバインディングを書き込む
    std::fs::write(&output_bindings_path, bindings.to_string()).expect("failed to write bindings");

    // Linux x86_64 のみ libvpl を CMake で static build してリンクする
    if is_target_linux_x86_64() {
        build_and_link_libvpl(&clone_dir);
    }
}

// ビルド対象が Linux x86_64 かどうかを返す
//
// build.rs はビルド対象ではなくホスト向けにコンパイルされるため、cfg! では
// クロスビルド時にビルド対象を判定できない。Cargo が設定する環境変数を使う。
fn is_target_linux_x86_64() -> bool {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS is not set");
    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH is not set");
    target_os == "linux" && target_arch == "x86_64"
}

// GitHub から libvpl ヘッダをクローンして api/ ディレクトリのパスを返す
fn clone_vpl_headers(clone_dir: &Path, version: &str, url: &str) -> PathBuf {
    let include_dir = clone_dir.join("api");

    // 既にクローン済みなら mfx.h の存在をチェックしてスキップする
    if include_dir.join("vpl/mfx.h").exists() {
        return include_dir;
    }

    // 不完全なクローンが残っている場合は削除する
    if clone_dir.exists() {
        std::fs::remove_dir_all(clone_dir).expect("failed to remove incomplete clone directory");
    }

    let output = std::process::Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "--branch",
            &format!("v{version}"),
            url,
            clone_dir.to_str().expect("invalid path"),
        ])
        .output()
        .expect("failed to run git clone");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("git clone libvpl failed: {}", stderr);
    }

    include_dir
}

// libvpl を CMake で static build してリンク指示を出力する
fn build_and_link_libvpl(clone_dir: &Path) {
    shiguredo_cmake::set_cmake_env();

    let dst = shiguredo_cmake::Config::new(clone_dir)
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("BUILD_TESTS", "OFF")
        .define("BUILD_EXAMPLES", "OFF")
        .define("INSTALL_EXAMPLES", "OFF")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .build();

    println!("cargo::rustc-link-search=native={}/lib", dst.display());
    println!("cargo::rustc-link-lib=static=vpl");
    println!("cargo::rustc-link-lib=dylib=stdc++");
    println!("cargo::rustc-link-lib=dylib=pthread");
    println!("cargo::rustc-link-lib=dylib=dl");
}

// Cargo.toml から VPL のバージョンと URL を取得する
fn get_metadata() -> (String, String) {
    let cargo_toml =
        shiguredo_toml::from_str(include_str!("Cargo.toml")).expect("failed to parse Cargo.toml");
    let vpl = shiguredo_toml::Value::Table(cargo_toml)
        .get("package")
        .and_then(|v| v.get("metadata"))
        .and_then(|v| v.get("external-dependencies"))
        .and_then(|v| v.get("vpl"))
        .expect("Cargo.toml does not contain [package.metadata.external-dependencies.vpl]")
        .clone();

    let version = vpl
        .get("version")
        .and_then(|s| s.as_str())
        .expect("vpl.version is missing")
        .to_string();

    let url = vpl
        .get("url")
        .and_then(|s| s.as_str())
        .expect("vpl.url is missing")
        .to_string();

    (version, url)
}

#[derive(Debug)]
struct CustomCallbacks;

impl bindgen::callbacks::ParseCallbacks for CustomCallbacks {
    fn add_derives(&self, _info: &bindgen::callbacks::DeriveInfo<'_>) -> Vec<String> {
        vec![]
    }
}

// docs.rs 向けビルドで出力するバインディングのダミー定義
//
// docs.rs のビルド環境はネットワークに接続できないため、libvpl のヘッダを取得できない。
// cargo doc は関数本体を型チェックしないため、ドキュメント生成にはアイテムの
// シグネチャに現れる型と、const 初期化子で参照する定数だけを定義すればよい。
const DOCS_RS_DUMMY_BINDINGS: &str = r"pub type mfxU16 = ::std::os::raw::c_ushort;
pub type mfxChar = ::std::os::raw::c_char;
pub type mfxHDL = *mut ::std::os::raw::c_void;
pub type mfxLoader = *mut _mfxLoader;
pub type mfxSession = *mut _mfxSession;
pub type mfxSyncPoint = *mut _mfxSyncPoint;
pub type mfxStatus = ::std::os::raw::c_int;

pub struct _mfxLoader;
pub struct _mfxSession;
pub struct _mfxSyncPoint;

#[derive(Copy, Clone)]
pub struct mfxBitstream;
#[derive(Copy, Clone)]
pub struct mfxEncodeCtrl;
#[derive(Copy, Clone)]
pub struct mfxEncodeStat;
#[derive(Copy, Clone)]
pub struct mfxExtBuffer;
#[derive(Copy, Clone)]
pub struct mfxExtCodingOption2;
#[derive(Copy, Clone)]
pub struct mfxExtCodingOption3;
#[derive(Copy, Clone)]
pub struct mfxExtVP9Param;
#[derive(Copy, Clone)]
pub struct mfxFrameData;
#[derive(Copy, Clone)]
pub struct mfxFrameInfo;
#[derive(Copy, Clone)]
pub struct mfxFrameSurface1;
#[derive(Copy, Clone)]
pub struct mfxVideoParam;
#[derive(Copy, Clone)]
pub struct mfxImplDescription;
#[derive(Copy, Clone)]
pub struct mfxDecoderDescription;
#[derive(Copy, Clone)]
pub struct mfxEncoderDescription;
#[derive(Copy, Clone)]
pub struct mfxEncoderDescription_encoder;

pub const mfxStatus_MFX_ERR_NONE: mfxStatus = 0;
pub const mfxStatus_MFX_ERR_UNKNOWN: mfxStatus = -1;
pub const mfxStatus_MFX_ERR_NULL_PTR: mfxStatus = -2;
pub const mfxStatus_MFX_ERR_UNSUPPORTED: mfxStatus = -3;
pub const mfxStatus_MFX_ERR_MEMORY_ALLOC: mfxStatus = -4;
pub const mfxStatus_MFX_ERR_NOT_ENOUGH_BUFFER: mfxStatus = -5;
pub const mfxStatus_MFX_ERR_INVALID_HANDLE: mfxStatus = -6;
pub const mfxStatus_MFX_ERR_LOCK_MEMORY: mfxStatus = -7;
pub const mfxStatus_MFX_ERR_NOT_INITIALIZED: mfxStatus = -8;
pub const mfxStatus_MFX_ERR_NOT_FOUND: mfxStatus = -9;
pub const mfxStatus_MFX_ERR_MORE_DATA: mfxStatus = -10;
pub const mfxStatus_MFX_ERR_MORE_SURFACE: mfxStatus = -11;
pub const mfxStatus_MFX_ERR_ABORTED: mfxStatus = -12;
pub const mfxStatus_MFX_ERR_DEVICE_LOST: mfxStatus = -13;
pub const mfxStatus_MFX_ERR_INCOMPATIBLE_VIDEO_PARAM: mfxStatus = -14;
pub const mfxStatus_MFX_ERR_INVALID_VIDEO_PARAM: mfxStatus = -15;
pub const mfxStatus_MFX_ERR_UNDEFINED_BEHAVIOR: mfxStatus = -16;
pub const mfxStatus_MFX_ERR_DEVICE_FAILED: mfxStatus = -17;
pub const mfxStatus_MFX_ERR_MORE_BITSTREAM: mfxStatus = -18;
pub const mfxStatus_MFX_ERR_GPU_HANG: mfxStatus = -21;
pub const mfxStatus_MFX_ERR_REALLOC_SURFACE: mfxStatus = -22;
pub const mfxStatus_MFX_ERR_RESOURCE_MAPPED: mfxStatus = -23;
pub const mfxStatus_MFX_ERR_NOT_IMPLEMENTED: mfxStatus = -24;
pub const mfxStatus_MFX_ERR_MORE_EXTBUFFER: mfxStatus = -25;
pub const mfxStatus_MFX_WRN_IN_EXECUTION: mfxStatus = 1;
pub const mfxStatus_MFX_WRN_DEVICE_BUSY: mfxStatus = 2;
pub const mfxStatus_MFX_WRN_VIDEO_PARAM_CHANGED: mfxStatus = 3;
pub const mfxStatus_MFX_WRN_PARTIAL_ACCELERATION: mfxStatus = 4;
pub const mfxStatus_MFX_WRN_INCOMPATIBLE_VIDEO_PARAM: mfxStatus = 5;
pub const mfxStatus_MFX_WRN_VALUE_NOT_CHANGED: mfxStatus = 6;
pub const mfxStatus_MFX_WRN_OUT_OF_RANGE: mfxStatus = 7;
pub const mfxStatus_MFX_WRN_FILTER_SKIPPED: mfxStatus = 10;
pub const mfxStatus_MFX_WRN_ALLOC_TIMEOUT_EXPIRED: mfxStatus = 13;

pub const MFX_FRAMETYPE_I: u32 = 1;
pub const MFX_FRAMETYPE_P: u32 = 2;
pub const MFX_FRAMETYPE_B: u32 = 4;
pub const MFX_FRAMETYPE_S: u32 = 8;
pub const MFX_FRAMETYPE_REF: u32 = 64;
pub const MFX_FRAMETYPE_IDR: u32 = 128;
pub const MFX_GOP_CLOSED: u32 = 1;
pub const MFX_GOP_STRICT: u32 = 2;
";
