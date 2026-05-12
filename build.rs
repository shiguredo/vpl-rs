use std::path::{Path, PathBuf};

fn main() {
    // Cargo.toml か build.rs が更新されたら、バインディングファイルを再生成する
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=build.rs");

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

    // src/bindings.rs にバインディングを書き込む
    std::fs::write(&output_bindings_path, bindings.to_string()).expect("failed to write bindings");

    // docs.rs ビルドではライブラリのビルドとリンクをスキップする
    if std::env::var("DOCS_RS").is_ok() {
        return;
    }

    // Linux のみ libvpl を CMake で static build してリンクする
    if cfg!(target_os = "linux") {
        build_and_link_libvpl(&clone_dir);
    }
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
