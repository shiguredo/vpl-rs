# 複数 Intel GPU 環境で使用するアダプタを選択できるようにする

Created: 2026-05-12
Model: Opus 4.7

## 背景とユースケース

`Encoder` / `Decoder` を作成するとき、`VplLibrary::create_session` (`src/lib.rs`) は `MFXCreateSession(loader, 0, &mut session)` のように実装インデックスを `0` に固定している。複数の Intel GPU を搭載したホストでも常に先頭のアダプタしか使えず、複数 `Encoder` / `Decoder` を作っても同じアダプタを共有することになる。

想定するユースケース:

- 1 ホストに Intel Arc を 3 枚挿し、N 個のセッションを物理 GPU に振り分けて並列にエンコード／デコードする
- iGPU と dGPU を併用し、用途に応じて使い分ける（例: dGPU で配信用エンコード、iGPU でデコード）

これらは現状の API では実現できない。

## 採用するアプローチ

libvpl の `MFXSetConfigFilterProperty` に **DRM render node 番号** をフィルタとして渡し、`MFXCreateSession(loader, 0, ...)` で開く方式を採用する。

理由:

- `MFXCreateSession` の第 2 引数 `i` は「フィルタ通過後の実装一覧のインデックス」であり、物理アダプタ番号ではない。libvpl のディスパッチャは同一物理アダプタを複数の実装エントリ（VA-API バックエンド + sub-device 別など）として列挙し得るため、`i` を単純に増やしても「N 番目の GPU」を選べる保証はない
- DRM render node 番号 (`/dev/dri/renderD<N>` の `N`、通常 128 以上) は物理アダプタを Linux 上で一意に識別する。libvpl やドライバの更新で実装の列挙順が変わっても DRM render node 番号は変わらない

vpl-rs は現状 Linux 専用（`README.md` の動作要件、`src/codec_info.rs` の `#[cfg(target_os = "linux")]`）。本 issue でも Linux 専用で実装する。

### libvpl 側のサポート根拠

libvpl 本体のディスパッチャは `"mfxExtendedDeviceId.DRMRenderNodeNum"` というプロパティ名を正式に受理する。

- `libvpl/src/mfx_dispatcher_vpl_config.cpp:713-714`: プロパティ名 `"DRMRenderNodeNum"` を `ePropExtDev_DRMRenderNodeNum` にマップ
- `libvpl/src/mfx_dispatcher_vpl_config.cpp:1300-1311`: 比較ロジック。`libImplExtDevID->DRMRenderNodeNum != 0` の実装エントリだけが比較対象。`0` のエントリはフィルタ通過しない
- `libvpl/src/mfx_dispatcher_vpl_loader.cpp` の `LoaderCtxVPL::CreateSession` がフィルタにマッチしないと `MFX_ERR_NOT_FOUND` を返す
- `libvpl/test/unit/src/dispatcher_common.cpp:393-411`: テストヘルパー `SetConfigFilterProperty<mfxU32>(loader, "mfxExtendedDeviceId.DRMRenderNodeNum", 130)` 経由で `MFXSetConfigFilterProperty` を呼び、`MFXCreateSession` が成功する単体テスト
- `libvpl/test/unit/src/dispatcher_common.cpp:413-440`: 該当 render node 番号が存在しない場合に `MFX_ERR_NOT_FOUND` を返す単体テスト

## API 設計

### `crate::adapter` モジュール（新規）

```rust
/// VPL ローダーで列挙される Intel HW 実装の情報
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AdapterInfo {
    /// `/dev/dri/renderD<N>` の `N`（通常 128 以上）。`AdapterSelector::DrmRenderNode` に渡す
    pub drm_render_node: u32,
    /// 実装名（NUL 終端を除去した UTF-8 文字列。例: "mfx-gen"。取得できない場合は空文字列）
    pub impl_name: String,
    /// 人間向けの GPU 名（例: "Intel(R) Arc(TM) A310 Graphics"）。取得できない場合は空文字列
    pub device_name: String,
    /// PCI device ID（例: Arc A310 は 0x56a6）
    pub pci_device_id: u16,
    /// PCI アドレス
    pub pci_address: PciAddress,
    /// integrated か discrete か
    pub media_adapter_type: MediaAdapterType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PciAddress {
    pub domain: u32,
    pub bus: u32,
    pub device: u32,
    pub function: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaAdapterType {
    Integrated,
    Discrete,
    Unknown,
}

/// Encoder / Decoder のセッションを開くときの対象アダプタ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdapterSelector {
    /// `/dev/dri/renderD<N>` の `N`
    DrmRenderNode(u32),
}

impl AdapterSelector {
    /// アダプタ指定値の入力検証
    ///
    /// `DrmRenderNode(0)` は libvpl 上「未設定」を意味する値のため Err を返す。
    /// `Encoder::new` / `Decoder::new` / `supported_codecs` から共通で呼ぶ。
    pub(crate) fn validate(self) -> Result<(), Error>;
}

/// 利用可能な Intel HW 実装を列挙する
///
/// 同一 DRM render node に対する重複エントリは除去し、`drm_render_node` 昇順で返す。
/// HW 実装が見つからないことはエラーではなく、空の `Vec` を返す。
pub fn list_adapters() -> Result<Vec<AdapterInfo>, Error>;
```

`PciAddress` の各フィールドは libvpl のヘッダ（`mfxExtendedDeviceId` の `PCIDomain` / `PCIBus` / `PCIDevice` / `PCIFunction` がすべて `mfxU32`）に揃えて `u32` とする。

`EncoderConfig` / `DecoderConfig` にも `#[non_exhaustive]` を付ける（クレート外部からのリテラル生成は不可になるが、既存テストや `README.md` の `config.target_kbps = Some(...)` のようなフィールド直接代入は引き続き可能）。

### `AdapterInfo` の各フィールドの取得元

`mfxImplDescription` と `mfxExtendedDeviceId` は別構造体で、`MFXEnumImplementations` を異なる `mfxImplCapsDeliveryFormat` で 2 回呼んで取得する。境界を取り違えないよう対応関係を明示する。

| フィールド | 取得元 | 取得 API |
|---|---|---|
| `impl_name` | `mfxImplDescription.ImplName` | `MFX_IMPLCAPS_IMPLDESCSTRUCTURE` |
| `media_adapter_type` | `mfxImplDescription.Dev.MediaAdapterType` | `MFX_IMPLCAPS_IMPLDESCSTRUCTURE` |
| `drm_render_node` | `mfxExtendedDeviceId.DRMRenderNodeNum` | `MFX_IMPLCAPS_DEVICE_ID_EXTENDED` |
| `device_name` | `mfxExtendedDeviceId.DeviceName` | `MFX_IMPLCAPS_DEVICE_ID_EXTENDED` |
| `pci_device_id` | `mfxExtendedDeviceId.DeviceID` | `MFX_IMPLCAPS_DEVICE_ID_EXTENDED` |
| `pci_address` | `mfxExtendedDeviceId.{PCIDomain, PCIBus, PCIDevice, PCIFunction}` | `MFX_IMPLCAPS_DEVICE_ID_EXTENDED` |

### `list_adapters()` の実装手順

1. `MFXLoad` でローダーを取る
2. `MFXCreateConfig` → `MFXSetConfigFilterProperty(b"mfxImplDescription.Impl\0", MFX_IMPL_TYPE_HARDWARE)` で HW 実装のみに絞る
3. `i = 0, 1, ...` と進めながら `MFX_ERR_NOT_FOUND` が返るまで以下を繰り返す（`mfxImplDescription` と `mfxExtendedDeviceId` は **同一 `i`** でペア取得する。別の `i` で取ると別エントリの情報になり対応が崩れる）
   - `MFXEnumImplementations(loader, i, MFX_IMPLCAPS_IMPLDESCSTRUCTURE, &hdl)` で `mfxImplDescription` を取る
   - `MFXEnumImplementations(loader, i, MFX_IMPLCAPS_DEVICE_ID_EXTENDED, &hdl_ext)` で `mfxExtendedDeviceId` を取る。`MFX_ERR_UNSUPPORTED` を返した場合（古い実装で未対応）または `hdl_ext` が NULL の場合は、そのエントリ全体を捨てる
   - `DRMRenderNodeNum == 0` のエントリは捨てる
   - 同一 `DRMRenderNodeNum` の重複は最初に出てきたものを採用する
   - 各ハンドルは取得直後に `MFXDispReleaseImplDescription` で解放する（RAII ガード型を `src/adapter.rs` 内に置く）
4. `DRMRenderNodeNum` 昇順にソートして返す
5. `MFXUnload` する

`mfxChar ImplName[MFX_IMPL_NAME_LEN]` / `DeviceName[MFX_STRFIELD_LEN]` は `CStr::from_bytes_until_nul` で読む。NUL が見つからない場合は `std::str::from_utf8` で配列長まで読み、不正 UTF-8 のときは空文字列にフォールバックする。`MFX_IMPL_NAME_LEN` / `MFX_STRFIELD_LEN` は `sys::` 経由で参照する。

`media_adapter_type` のマッピング: `sys::mfxMediaAdapterType_MFX_MEDIA_INTEGRATED` → `Integrated`、`sys::mfxMediaAdapterType_MFX_MEDIA_DISCRETE` → `Discrete`、それ以外（`MFX_MEDIA_UNKNOWN = 0xFFFF` を含む）はすべて `Unknown`。

`list_adapters()` は呼び出しごとに `MFXLoad` + 列挙 + `MFXUnload` を行う重い処理。利用側は通常アプリ起動時に 1 回だけ呼んで結果を保持することを想定する。本 issue ではキャッシュ層は導入しない。

### `EncoderConfig` / `DecoderConfig` のシグネチャ変更

`EncoderConfig` 構造体本体に `pub adapter: AdapterSelector` フィールドを先頭に追加し、`#[non_exhaustive]` を付ける。

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EncoderConfig {
    pub adapter: AdapterSelector,
    pub codec: CodecConfig,
    pub width: u32,
    pub height: u32,
    // ... 既存フィールドは順序維持
}

impl EncoderConfig {
    pub fn new(
        adapter: AdapterSelector,
        codec: CodecConfig,
        width: u32,
        height: u32,
        frame_format: FrameFormat,
        framerate_num: u32,
        framerate_den: u32,
        rate_control_mode: RateControlMode,
    ) -> Self;
}
```

`DecoderConfig` も同様に `adapter` フィールドを先頭に追加し、`new` を新設する。

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DecoderConfig {
    pub adapter: AdapterSelector,
    pub codec: DecoderCodec,
}

impl DecoderConfig {
    pub fn new(adapter: AdapterSelector, codec: DecoderCodec) -> Self;
}
```

### `VplLibrary::create_session` のシグネチャ変更

現状の `impl_type: mfxImplType` 引数は呼び出し元すべてが `MFX_IMPL_TYPE_HARDWARE` を渡しており、本 issue は Linux 専用かつ HW 専用が前提なので、この引数を削除する。

```rust
pub(crate) fn create_session(
    &self,
    adapter: AdapterSelector,
) -> Result<(sys::mfxLoader, sys::mfxSession), Error>;
```

`AdapterSelector::DrmRenderNode(n)` を受けたときの動作:

1. `adapter.validate()` を呼ぶ（`n == 0` の場合はここで Err を返す）
2. `MFXLoad` でローダーを取る
3. `MFXCreateConfig` で **1 つ目** の cfg を作り、`MFXSetConfigFilterProperty(cfg, b"mfxImplDescription.Impl\0", MFX_IMPL_TYPE_HARDWARE)` を設定する
4. `MFXCreateConfig` で **2 つ目** の cfg を作り、`MFXSetConfigFilterProperty(cfg, b"mfxExtendedDeviceId.DRMRenderNodeNum\0", n)` を設定する（`mfxVariantType_MFX_VARIANT_TYPE_U32`）。プロパティごとに別 cfg を作るのは libvpl の慣用（ヘッダ `mfxdispatcher.h` の `MFX_ADD_PROPERTY_U32` マクロが踏襲しているスタイル）に合わせる
5. `MFXCreateSession(loader, 0, &session)` で開く

`validate()` を呼ぶ責務は `create_session` と `supported_codecs` の 2 か所に集約する。`Encoder::new` / `Decoder::new` は `create_session` を経由するため、自身では `validate()` を呼ばない（二重検証を避ける）。`supported_codecs` は `create_session` を呼ばずに独自で `MFXLoad` ループを構成するため、自前で `validate()` を呼ぶ必要がある。

該当の render node が存在しない場合、libvpl は `MFXCreateSession` で `MFX_ERR_NOT_FOUND` を返す。`Error::from_mfx` で `status_code` / `status_name` / `function` を保持したまま、`status_message` だけを「`no Intel HW implementation found for DRM render node N`」（英語、`N` は実値）に差し替える。これを実現するため `Error` に以下のヘルパーを追加する。

```rust
impl Error {
    /// status_message のみを置き換え、status_code / status_name / function は変更しない
    pub(crate) fn with_message(mut self, message: String) -> Self {
        self.status_message = Some(Cow::Owned(message));
        self
    }
}
```

`pub(crate)` で十分（クレート外部からエラーを再ラベルする必要はない）。

## 影響範囲

- `src/lib.rs`: `VplLibrary::create_session` のシグネチャ変更（`impl_type` 引数を削除）、`adapter` モジュールの `pub` 再エクスポート、doctest の `EncoderConfig::new` 呼び出しを新シグネチャに更新。doctest 内ではアダプタを `list_adapters()` の戻り値先頭から取り出す形（実機がなくても `no_run` でコンパイルさえ通ればよい）で記述する
- `src/encode.rs`: `EncoderConfig` への `adapter` フィールドと `#[non_exhaustive]` 追加、`EncoderConfig::new` のシグネチャ変更、`Encoder::new` 内で `create_session` に `config.adapter` を渡す
- `src/decode.rs`: `DecoderConfig` への `adapter` フィールドと `#[non_exhaustive]` 追加、`DecoderConfig::new` の新設、`Decoder::new` 内で `create_session` に `config.adapter` を渡す
- `src/codec_info.rs`: `supported_codecs` を `pub fn supported_codecs(adapter: AdapterSelector) -> Result<Vec<CodecInfo>, Error>` に変更し、内部で `adapter.validate()` と DRM render node フィルタ設定を行う。戻り値型 `Vec<CodecInfo>` は変更しない（複数アダプタを一覧したい呼び出し側は `list_adapters().iter().map(|a| supported_codecs(AdapterSelector::DrmRenderNode(a.drm_render_node)))` で組み合わせる）
- `src/error.rs`: `Error::with_message` ヘルパーを追加
- `src/adapter.rs`: 新規ファイル。`AdapterSelector` / `AdapterInfo` / `PciAddress` / `MediaAdapterType` / `list_adapters` / `AdapterSelector::validate` / RAII ガード型を含む
- `README.md`: 使い方サンプル（`## 使い方` 配下）を新シグネチャに更新し、`list_adapters()` の使用例を追加する
- `tests/common/mod.rs`: 新規。`pub fn test_adapter() -> AdapterSelector` などテスト間で共有するヘルパーを置く（Rust の統合テストは各ファイルが独立クレートのため、`tests/test_adapter.rs` と `tests/test_roundtrip.rs` でヘルパー関数を直接共有できない。`tests/common/mod.rs` を作って各ファイルから `mod common;` で取り込む）。一部のヘルパーが特定ファイルからしか使われない場合に未使用警告で `-D warnings` を踏まないよう、`tests/common/mod.rs` の冒頭に `#![allow(dead_code)]` を付ける
- `tests/test_roundtrip.rs`: `EncoderConfig::new` 呼び出しと `DecoderConfig` 生成箇所、合計 10 か所を新シグネチャに更新する
- `CHANGES.md`: `## develop` セクションに以下を追加（`[ADD]` → `[CHANGE]` の順）

  ```
  - [ADD] shiguredo_vpl::adapter::list_adapters と AdapterSelector / AdapterInfo を追加する
    - @実装者の GitHub ユーザー名
  - [CHANGE] EncoderConfig::new / DecoderConfig::new / codec_info::supported_codecs にアダプタ指定を必須化する
    - @実装者の GitHub ユーザー名
  ```

  `@実装者の GitHub ユーザー名` は実装時に既存 `CHANGES.md` (例: `@melpon`) と同じ形式で置き換える。

`src/sys.rs` 経由で参照する bindgen シンボル（`MFXEnumImplementations`、`MFXDispReleaseImplDescription`、`MFX_IMPLCAPS_DEVICE_ID_EXTENDED`、`mfxExtendedDeviceId`、`MFX_MEDIA_INTEGRATED` / `MFX_MEDIA_DISCRETE`）はいずれも現状の `build.rs` の bindgen 出力に含まれている。bindgen 設定の変更は不要。

## テスト戦略

- `tests/test_adapter.rs`（新規、`src/adapter.rs` 対応）
  - `list_adapters()` の戻り値が `drm_render_node` 昇順かつ重複なしであること
  - `AdapterSelector::DrmRenderNode(0)` を `Encoder::new` / `Decoder::new` / `supported_codecs` のいずれに渡しても `Err` が返ること（`Encoder::new` / `Decoder::new` のテストでは width / height / framerate に正常値（例: 1920x1080, 30/1）を併用し、アダプタ以外の入力検証を先に通過させる）
  - 存在しない render node 番号（例: `u32::MAX`）を渡すと `Err` が返り、`Error::status_name()` が `Some("MFX_ERR_NOT_FOUND")` を返し、`Error::status_message()` に該当の番号が含まれること
- `tests/test_roundtrip.rs`
  - `tests/common/mod.rs::test_adapter()` を介してテストアダプタを取得し、10 か所の `EncoderConfig::new` / `DecoderConfig` 生成で使い回す
  - `test_adapter()` は内部で `list_adapters()` の結果をキャッシュし、同一テストバイナリ内で `MFXLoad` が繰り返されないようにする。Rust の統合テストは `tests/test_*.rs` ごとに別バイナリのため、キャッシュ効果はファイル単位に閉じる
  - `list_adapters()` が空 Vec を返す環境（Intel HW 実装が見つからない＝非 Intel GPU またはドライバ未導入。本クレートは libvpl 2.16.0 を静的リンクするため `MFX_IMPLCAPS_DEVICE_ID_EXTENDED` 未対応の古い libvpl を引くケースは存在しない）では実機テストは成立しないため、`test_adapter()` は `expect("Intel HW アダプタが見つからない")` で panic させる（AGENTS.md「テストメッセージは全て日本語」に従う）。CI 環境はテスト前提として最低 1 つのアダプタが見つかる構成にする

`AdapterInfo` などを `#[non_exhaustive]` にした影響で、`tests/` 配下（Rust 仕様上は別クレート）から構造体リテラルを作れないため、複数アダプタを必要とする不変条件の検証は実機が複数アダプタを持つ環境での手動テスト扱いとする。CI 環境は単一アダプタ前提で、ユニットテストでは正常系と 1 アダプタでも検証可能なエラーパスのみカバーする。

## 完了条件

- [ ] `src/adapter.rs` を新設し `AdapterSelector` / `AdapterInfo` / `PciAddress` / `MediaAdapterType` / `list_adapters` / `AdapterSelector::validate` を実装した
- [ ] `AdapterSelector` / `AdapterInfo` / `PciAddress` / `MediaAdapterType` / `EncoderConfig` / `DecoderConfig` に `#[non_exhaustive]` を付けた
- [ ] `EncoderConfig::new` / `DecoderConfig::new` / `codec_info::supported_codecs` のシグネチャに `adapter: AdapterSelector` を追加した
- [ ] `VplLibrary::create_session` および `supported_codecs` が `adapter.validate()` 経由で `DrmRenderNode(0)` を即時拒否し、DRM render node 番号によるフィルタを設定するようにした
- [ ] `Error::with_message` を追加し、`MFX_ERR_NOT_FOUND` 時に該当 render node 番号を含むメッセージを返すようにした
- [ ] `src/lib.rs` の doctest と `README.md` の使い方サンプルを新シグネチャに更新した
- [ ] `tests/common/mod.rs` を新設し `test_adapter()` で `list_adapters()` の結果をキャッシュした
- [ ] `tests/test_roundtrip.rs` の Encoder / Decoder 生成 10 か所を新シグネチャに更新した
- [ ] `tests/test_adapter.rs` を追加し、`0` 指定と存在しない render node のエラーパスを単体テストでカバーした
- [ ] `CHANGES.md` に `[ADD]` → `[CHANGE]` の順でエントリを追加した
- [ ] `feature/change-multi-adapter-session-support` ブランチで作業し、対象ブランチへマージした
- [ ] issue ファイルに `Completed: YYYY-MM-DD` を追記し、`issues/closed/` へ `git mv` した
- [ ] `cargo test` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --check` / prek のフックが通る

## 参考資料

- libvpl ヘッダ
  - `api/vpl/mfxdispatcher.h`: `MFXCreateSession`、`MFXEnumImplementations`、`MFXDispReleaseImplDescription`
  - `api/vpl/mfxcommon.h`: `mfxImplDescription`、`mfxExtendedDeviceId`、`mfxImplCapsDeliveryFormat`、`mfxMediaAdapterType`

ディスパッチャ実装と単体テストの該当行は `## 採用するアプローチ` 配下の「libvpl 側のサポート根拠」で列挙したものを実装時にコードコメントから参照する。参照バージョンは `Cargo.toml` の `package.metadata.external-dependencies.vpl.version`。
