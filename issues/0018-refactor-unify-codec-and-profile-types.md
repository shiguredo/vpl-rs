# プロファイル enum とコーデック識別型が重複しているため統合する

- Priority: Medium
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/refactor-unify-codec-and-profile-types
- Polished: 2026-08-02

## 目的

コーデックプロファイル型（`H264Profile` / `H264EncodingProfile` 等の 4 コーデック × 2 バージョン計 8 型）が意味論的に同じ概念を表しているのに別の型として並立している。またコーデック識別子として `VideoCodecType` と `DecoderCodec` の 2 型が並立している。MFX_PROFILE_* への写像も 2 箇所で二重実装。将来プロファイル追加時に 2 箇所同期が必須で漏れやすいため、統合してリスクを排除する。

## 優先度根拠

Medium。以下による。

- **公開 API の破壊的変更**: 統合には API 名変更が伴うため、`skills/shiguredo-vpl/SKILL.md` の「良い設計のためには破壊的変更を積極的に行う」方針に沿えるが、リリース調整は必要。
- **将来のプロファイル追加時のバグ源**: `codec_info` モジュール側 / `encode` モジュール側でプロファイルを追加し忘れると、`supported_codecs()` の返り値と `EncoderConfig::codec` の設定可能範囲が不整合になる。
- **保守負荷**: 2 箇所で同じ enum を管理するコスト。命名（`H264Profile::High10` と `H264EncodingProfile::High10` が別の型）による混乱リスク。
- **Priority は Medium**: 現状動いており、直接的なバグはない。設計負債の返済として次のリファクタで統合する優先度。

## 現状

### プロファイル型の完全重複

`src/codec_info.rs` の `H264EncodingProfile` / `HevcEncodingProfile` / `Vp9EncodingProfile` / `Av1EncodingProfile` と、`src/encode.rs` の `H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` は variant が完全一致。

例: H.264

- `src/encode.rs` の `H264Profile`:

```rust
pub enum H264Profile {
    Baseline,
    ConstrainedBaseline,
    Main,
    High,
    ConstrainedHigh,
    High10,
    High422,
}
```

- `src/codec_info.rs` の `H264EncodingProfile`:

```rust
pub enum H264EncodingProfile {
    Baseline,
    ConstrainedBaseline,
    Main,
    High,
    ConstrainedHigh,
    High10,
    High422,
}
```

variant は 7 個全て同一。同様に HEVC (5 variants) / VP9 (4 variants) / AV1 (1 variant) も完全に一致。

### MFX_PROFILE_* への写像の二重実装

- `src/encode.rs` の `codec_profile()`（`H264Profile::High10` → `sys::MFX_PROFILE_AVC_HIGH10` など）
- `src/codec_info.rs` の `query_encoding_profiles()`（`sys::MFX_PROFILE_AVC_HIGH10` → `H264EncodingProfile::High10` など）

同じ写像を 2 箇所で管理している。将来 `H264Profile::High444` などを追加する場合、両方に追加が必要。

### コーデック識別型の並立

- `src/codec_info.rs` の `VideoCodecType`（H264 / Hevc / Vp9 / Av1）
- `src/decode.rs` の `DecoderCodec`（H264 / Hevc / Vp9 / Av1）

`VideoCodecType` と `DecoderCodec` は variant が完全一致（プロファイルなど付随情報なし）。`src/encode.rs` の `CodecConfig`（variants はコーデック識別 + 設定情報）はコーデック識別に加えて `H264EncoderConfig` 等の設定情報を持つ別種の型であり、統合対象の重複ではない（名前は維持する。理由は「設計方針」参照）。

## 設計方針

### プロファイル統合

`H264Profile` に統一し、`codec_info::EncodingProfiles::H264(Vec<H264Profile>)` とする。HEVC / VP9 / AV1 も同様。

- `H264EncodingProfile` を削除し `H264Profile` を再利用
- `codec_profile()` と `query_encoding_profiles()` は同じ写像を共有する（例: `H264Profile::to_mfx_profile()` / `H264Profile::from_mfx_profile()` を実装）

写像メソッドの詳細:

- `fn to_mfx_profile(self) -> u32` と `fn from_mfx_profile(id: u32) -> Option<Self>` を `pub(crate)` で実装する。`from_mfx_profile()` は定義モジュール（encode）外の `query_encoding_profiles()`（codec_info）から使用されるため `pub(crate)` が必須。`to_mfx_profile()` は encode 内の `codec_profile()` からのみ使用されるため private でも成立するが、写像メソッド群の対称性のため両方 `pub(crate)` にする
- `codec_profile()` は現状 u16 を返す（`mfxInfoMFX.CodecProfile` は u16）ため、`to_mfx_profile()` の戻り値（u32）を `as u16` でキャストして使用する
- `from_mfx_profile()` は未知のプロファイル ID に対して `None` を返し、`query_encoding_profiles()` 側は `filter_map` でスキップする（現行の `match_profiles` の「未知の profile ID をスキップする」挙動を維持する）
- `codec_profile()` の現行の `None → MFX_PROFILE_UNKNOWN` 分岐（プロファイル未指定時）は `Option<H264Profile>` を受け取る関数として維持する

### コーデック識別型統合

`VideoCodecType` を crate 全体で共有する識別子として位置付ける。

- `DecoderCodec` を削除し、`DecoderConfig::codec: VideoCodecType` にする
- `VideoCodecType::to_codec_id()` は現在 `#[cfg(target_os = "linux")]` でゲートされた impl 内の private メソッドのため、decode モジュールから呼べるようにする必要がある。**ゲートを外し、可視性を `pub(crate)` に変更する**（「非 Linux 向けの `to_codec_id()` を追加する」選択肢は存在しない。依存 issue 0015 適用後は非 Linux 自体が `compile_error!` でビルド拒否されるため）
- `CodecConfig` はそのままの名前を維持する（コーデック識別に加えてエンコード設定（プロファイル等）を保持する型であり、純粋な識別子である `VideoCodecType` とは役割が異なる。改名しても test_roundtrip.rs の十数箇所（16 箇所）+ test_adapter.rs の 4 箇所の修正コストに見合わない）

### 一括置き換え

次のリリースで `[CHANGE]` として一括置き換え（`#[deprecated]` は残さない）。vpl-rs は破壊的変更を積極的に行う方針であり、deprecated を残すコストのほうが高い。

### プロファイル型の所属モジュール

統合後のプロファイル型（`H264Profile` 等）は `src/encode.rs` に残す。プロファイル型は `EncoderConfig` のプロファイル指定（`H264EncoderConfig.profile: Option<H264Profile>` 等）で使用され、エンコード設定の一部であるため encode モジュールが自然な所属である。`src/codec_info.rs` の `EncodingProfiles` は `use crate::encode::H264Profile` で参照する（依存方向: codec_info → encode → sys）。

なお、依存 issue 0020（encode.rs のサブモジュール分割）適用後は、プロファイル型は分割後の `src/encode/config.rs` に置かれることになる。**適用順序は本 issue (0018) を先に適用し、その差分の上に 0020 の変更を重ねる**（0020 側も「0018 を先に適用し、その差分の上に本 issue の変更を重ねる」と明記している）。

### コーデック ID 写像の二重実装（本 issue の対象外）

`CodecConfig` の `codec_id()`（encode モジュール）と `VideoCodecType::to_codec_id()`（codec_info モジュール）のコーデック ID 写像も 2 箇所に存在するが、`codec_id()` は `CodecConfig` の設定情報を含む match であり、`to_codec_id()` への共通化は `CodecConfig` の統合（本 issue では行わない）とセットで行うため、本 issue の対象外とする。

### `EncodingProfiles::None` の扱い

`EncodingProfiles::None` variant（エンコード非対応時の返り値）は統合後も維持する（`probe_encoding` がエンコード非対応時に返す値のため）。

### `#[non_exhaustive]` 削除（分離候補）

`src/decode.rs` の `DecoderConfig` に付与されている `#[non_exhaustive]` は shiguredo-rust 規約違反である。ただし、`src/encode.rs` の `EncoderConfig` にも同じ `#[non_exhaustive]` が付与されており、「規約違反の解消」として片方だけ削除するのは不完全である。`DecoderConfig` の `#[non_exhaustive]` 削除は本 issue の目的（型の統合）と無関係な別目的の作業であるため、**分離候補**として「公開型の `#[non_exhaustive]` を削除して shiguredo-rust 規約に準拠する」別 issue に切り出すことを検討する（`EncoderConfig` も含めて対応）。この記述はユーザー確認を経て本 issue から削除するか、本 issue の対象に含めるかを決定する。

## 完了条件

以下すべてを満たす。

1. `H264EncodingProfile` / `HevcEncodingProfile` / `Vp9EncodingProfile` / `Av1EncodingProfile` を削除し、`H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` を crate 全体で再利用する。
2. `EncodingProfiles::H264(Vec<H264Profile>)` などに置き換わる（`EncodingProfiles::None` は維持）。
3. `codec_profile()` と `query_encoding_profiles()` が共通の写像を使う（`pub(crate)` の `to_mfx_profile(self) -> u32` / `from_mfx_profile(id: u32) -> Option<Self>` メソッド化。u16/u32 のキャストと未知 ID のスキップ挙動の維持を含む）。
4. `DecoderCodec` を削除し、`DecoderConfig::codec: VideoCodecType` に変更する。`VideoCodecType::to_codec_id()` は `#[cfg(target_os = "linux")]` ゲートを外し、可視性を `pub(crate)` に変更する（`Decoder` 構造体の `codec` フィールドと `initialize()` の `self.codec.codec_id()` 呼び出しを含む）。**ゲート外しの対象に注意**: `src/codec_info.rs` の `use crate::{AdapterSelector, Error, sys}` も `#[cfg(target_os = "linux")]` でゲートされているため、`to_codec_id()` が `sys::MFX_CODEC_*` を参照できるよう、この `use` のゲートも外す。一方 `fn all()` は Linux 専用の `build_codec_info_list` からのみ使用されるため `#[cfg(target_os = "linux")]` を維持する（非 Linux は `compile_error!` でビルド拒否されるため警告は発生しないが、使用関係を明確にするため）。
5. `src/lib.rs` の `pub use` を新型に合わせて更新する（doc comment 内のデコード例（`DecoderCodec` 参照）も更新する。更新漏れだと doctest がコンパイルエラーになる）。
6. `tests/test_roundtrip.rs` と `tests/test_adapter.rs` を新型に追随させる（`DecoderCodec` は test_adapter.rs にも使用箇所がある）。
7. `README.md` / `skills/shiguredo-vpl/SKILL.md` の型名参照を更新する。具体的には、`README.md` のデコード表とサンプルの `DecoderCodec` 参照、`SKILL.md` の公開モジュール構成（「`decode` — ... `DecoderCodec` / ...」）とデコード例の `DecoderCodec` 参照を更新し、`codec_info` モジュールの説明（「各コーデックプロファイル一覧型」）を統合後の構成（`EncodingProfiles` が encode 側の `H264Profile` 等を参照する形）に合わせて見直す。
8. `CHANGES.md` の `## develop` に `[CHANGE]` として破壊的変更を明記する。

## 影響範囲

- `src/encode.rs`（プロファイル / コーデック設定型の再編、`codec_profile()` の写像共通化）
- `src/decode.rs`（`DecoderCodec` 削除、`DecoderConfig` 変更、`Decoder` 構造体と `initialize()` の追随）
- `src/codec_info.rs`（`H264EncodingProfile` などの削除、`VideoCodecType` の再利用、`to_codec_id()` の可視性変更、`query_encoding_profiles()` の写像共通化）
- `src/lib.rs`（`pub use` と doc comment の更新）
- `tests/test_roundtrip.rs`（型追随）
- `tests/test_adapter.rs`（`DecoderCodec` を使っている箇所の追随）
- `README.md`（型名参照）
- `skills/shiguredo-vpl/SKILL.md`（型名参照）
- `CHANGES.md`

## 参考

- `skills/shiguredo-vpl/SKILL.md` の「良い設計のためには破壊的変更を積極的に行う」
- 過去の破壊的変更例: 2026.3.0 の Encoder/Decoder ハンドラー方式化（CHANGES.md の 2026.3.0 エントリ）
- 関連 issue: 0015（非 Linux ガード。適用後は非 Linux がビルド拒否されるため、`to_codec_id()` の非 Linux 向け検討は不要。**適用順序は 0015 を先に適用する**）
- 関連 issue: 0020（encode.rs のサブモジュール分割。**本 issue を先に適用し、その差分の上に 0020 の変更を重ねる**。0020 適用後はプロファイル型は `src/encode/config.rs` に置かれる）
- 関連 issue: 0008（Decoder の user_data 対応付け変更。`DecoderCodec` 型自体には触れないが、同じ `src/decode.rs` の `Decoder` 構造体と `tests/test_roundtrip.rs` の `decode()` ヘルパーを編集する（0008 は `frame_count` 追加とヘルパー拡張、本 issue は `codec` フィールドの型変更と引数型変更）。編集箇所が異なるため実質的な衝突リスクは低い。**適用順序は本 issue (0018) を先に適用する**）
- 関連 issue: 0019（VPL ローダーの `LoaderBuilder` 化。同じ `src/codec_info.rs` を編集する（0019 は `supported_codecs`、本 issue は `query_encoding_profiles` / `match_profiles`）ため、適用順序に注意）
