# プロファイル enum とコーデック識別型が重複しているため統合する

- Priority: Medium
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/refactor-unify-codec-and-profile-types
- Polished: {YYYY-MM-DD}

## 目的

コーデックプロファイル型（`H264Profile` / `H264EncodingProfile` 等の 4 コーデック × 2 バージョン計 8 型）と、コーデック識別型（`VideoCodecType` / `DecoderCodec` の 2 型）が意味論的に同じ概念を表しているのに別の型として並立している。MFX_PROFILE_* への写像も 2 箇所で二重実装。将来プロファイル追加時に 2 箇所同期が必須で漏れやすいため、統合してリスクを排除する。

## 優先度根拠

Medium。以下による。

- **公開 API の破壊的変更**: 統合には API 名変更が伴うため、`vpl-rs` の破壊的変更方針（SKILL.md L383「良い設計のためには破壊的変更を積極的に行う」）に沿えるが、リリース調整は必要。
- **将来のプロファイル追加時のバグ源**: `codec_info.rs` 側 / `encode.rs` 側でプロファイルを追加し忘れると、`supported_codecs()` の返り値と `EncoderConfig::codec` の設定可能範囲が不整合になる。
- **保守負荷**: 2 箇所で同じ enum を管理するコスト。命名（`H264Profile::High10` と `H264EncodingProfile::High10` が別の型）による混乱リスク。
- **Priority は Medium**: 現状動いており、直接的なバグはない。設計負債の返済として次のリファクタで統合する優先度。

## 現状

### プロファイル型の完全重複

`src/codec_info.rs:88-139` の `H264EncodingProfile` / `HevcEncodingProfile` / `Vp9EncodingProfile` / `Av1EncodingProfile` と、`src/encode.rs:9-60` の `H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` は variant が完全一致。

例: H.264

- `src/encode.rs:9-25`

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

- `src/codec_info.rs:88-104`

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

- `src/encode.rs:1476-1510` の `codec_profile()`（`H264Profile::High10` → `sys::MFX_PROFILE_AVC_HIGH10` など）
- `src/codec_info.rs:335-378` の `query_encoding_profiles()`（`sys::MFX_PROFILE_AVC_HIGH10` → `H264EncodingProfile::High10` など）

同じ写像を 2 箇所で管理している。将来 `H264Profile::High444` などを追加する場合、両方に追加が必要。

### コーデック識別型の三重定義

- `src/codec_info.rs:8-17` `VideoCodecType`（H264 / Hevc / Vp9 / Av1）
- `src/decode.rs:10-19` `DecoderCodec`（H264 / Hevc / Vp9 / Av1）
- `src/encode.rs:91-101` `CodecConfig`（variants はコーデック識別）

`VideoCodecType` と `DecoderCodec` は variant が完全一致（プロファイルなど付随情報なし）。`CodecConfig::H264(H264EncoderConfig)` はコーデック識別に加えて設定情報を持つが、識別部分は同じ意味。

## 設計方針

### プロファイル統合

`H264Profile` に統一し、`codec_info::EncodingProfiles::H264(Vec<H264Profile>)` とする。HEVC / VP9 / AV1 も同様。

- `H264EncodingProfile` を削除し `H264Profile` を再利用
- `codec_profile()` と `query_encoding_profiles()` は同じ写像テーブルを共有する（例: `H264Profile::to_mfx_profile()` / `H264Profile::from_mfx_profile()` を実装）

### コーデック識別型統合

`VideoCodecType` を crate 全体で共有する識別子として位置付ける。

- `DecoderCodec` を削除し、`DecoderConfig::codec: VideoCodecType` にする
- `CodecConfig` は `EncoderCodecConfig`（あるいは `CodecEncodeConfig`）に改名し、「コーデック識別 + エンコード固有設定」という位置付けを明示

または、`CodecConfig` はそのまま残して `CodecConfig` の中身を `VideoCodecType` と `H264EncoderConfig` などに分解する案もある（例: `pub struct CodecConfig { pub codec: VideoCodecType, pub h264: Option<H264EncoderConfig>, ... }`）。

推奨は前者（`CodecConfig` を `EncoderCodecConfig` に改名）。「エンコード時のコーデック設定」であることが名前で分かる。

### 段階的移行

破壊的変更のため、以下の 2 段階で進める案:

1. 統合後の型を追加し、`#[deprecated]` で旧型を残す（1 マイナー版）
2. 次のマイナー版で旧型を削除

または一括で置き換える案（SKILL.md L383 の方針に従い、こちらのほうが自然）:

- 次のリリース（2026.4.0 想定）で `[CHANGE]` として一括置き換え

推奨は **一括置き換え**。vpl-rs は破壊的変更を積極的に行う方針であり、`#[deprecated]` を残すコストのほうが高い。

## 完了条件

以下すべてを満たす。

1. `H264EncodingProfile` / `HevcEncodingProfile` / `Vp9EncodingProfile` / `Av1EncodingProfile` を削除し、`H264Profile` / `HevcProfile` / `Vp9Profile` / `Av1Profile` を crate 全体で再利用する。
2. `EncodingProfiles::H264(Vec<H264Profile>)` などに置き換わる。
3. `codec_profile()` と `query_encoding_profiles()` が共通の写像を使う（`impl H264Profile { fn to_mfx_profile(self) -> u32 ... fn from_mfx_profile(id: u32) -> Option<Self> ... }` などのメソッド化）。
4. `DecoderCodec` を削除し、`DecoderConfig::codec: VideoCodecType` に変更する。
5. `CodecConfig` を必要に応じて改名する（推奨: `EncoderCodecConfig`）。
6. `src/lib.rs` の `pub use` を新型に合わせて更新する。
7. `tests/test_roundtrip.rs` を新型に追随させる。
8. `README.md` / `SKILL.md` の型名参照を更新する。
9. `CHANGES.md` の `## develop` に `[CHANGE]` として破壊的変更を明記する。

## 影響範囲

- `src/encode.rs`（プロファイル / コーデック設定型の再編）
- `src/decode.rs`（`DecoderCodec` 削除、`DecoderConfig` 変更）
- `src/codec_info.rs`（`H264EncodingProfile` などの削除、`VideoCodecType` の再利用）
- `src/lib.rs`（`pub use` の更新）
- `tests/test_roundtrip.rs`（型追随）
- `tests/test_adapter.rs`（`DecoderCodec` を使っている箇所の追随）
- `README.md`（型名参照）
- `skills/shiguredo-vpl/SKILL.md`（型名参照）
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F11
- SKILL.md L383「良い設計のためには破壊的変更を積極的に行う」
- 過去の破壊的変更例: 2026.3.0 の Encoder/Decoder ハンドラー方式化（CHANGES.md L27-49）
