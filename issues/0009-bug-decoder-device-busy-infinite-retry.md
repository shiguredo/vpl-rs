# Decoder の DEVICE_BUSY / MORE_SURFACE リトライに上限がなく GPU 異常時にプロセスがハングする

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-decoder-device-busy-infinite-retry
- Polished: 2026-07-01

## 目的

`Decoder::decode` / `Decoder::finish` の内部ループが `MFX_WRN_DEVICE_BUSY` / `MFX_ERR_MORE_SURFACE` を返し続けた場合に **無限ループ / スピンループになるバグを修正する**。Encoder 側は `DEVICE_BUSY_MAX_RETRIES = 30` で上限を掛けているのに Decoder 側が退化しており、GPU ハング等の異常状態で Decoder を触ったプロセスがそのまま固まる。

## 優先度根拠

High。以下による。

- **プロセスハング**: GPU が異常状態になると `Decoder::decode` を呼んだスレッドがブロックしたままとなり、上位から検知できない。パニックも Error も返らない。
- **Encoder との実装非対称**: 同一ライブラリ内で Encoder は上限リトライ、Decoder は無限リトライ。同じ設計原則を採用しない理由がない。
- **`MORE_SURFACE` の CPU 100% スピン**: `sleep` すら入っていないため、`MORE_SURFACE` が返り続けると CPU 100% でスピンする。AV1 の `FilmGrain` 有効ストリームで実際に返り得る（libvpl 仕様）。
- **本番運用でのシャットダウン阻害**: Decoder を Drop しようとしても `stop_worker` の前に `finish` を呼んでいたら、無限ループ内から抜けられないため Drop も返らない。プロセス終了処理も回らない。

## 現状

### DEVICE_BUSY の無限リトライ（decode_bitstream）

`src/decode.rs:373-376`:

```rust
// DEVICE_BUSY: デバイスが混雑している。1ms 待って再試行
if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
    std::thread::sleep(std::time::Duration::from_millis(1));
    continue;
}
```

`while bs.DataLength > 0` ループの中で無限に `continue` する。リトライ上限なし。

### DEVICE_BUSY の無限リトライ（finish のドレイン）

`src/decode.rs:431-433`:

```rust
if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
    std::thread::sleep(std::time::Duration::from_millis(1));
    continue;
}
```

`finish()` の drain ループも同じく上限なし。`Decoder::finish` は「コールバックが呼び出され終わるまでブロックする」契約 (`src/decode.rs:405-407`) なので、BUSY が返り続けると `finish` 呼び出しが永久にブロックし、Drop から抜けることもできない。

### MORE_SURFACE の CPU 100% スピン

`src/decode.rs:370-372`:

```rust
// MORE_SURFACE: 内部割り当てでは通常発生しないが、安全のため再試行
if status == sys::mfxStatus_MFX_ERR_MORE_SURFACE {
    continue;
}
```

`sleep` すら入っていないため、`MORE_SURFACE` が返り続けると CPU 100% でスピンする。コメントは「通常発生しない」と書いているが、これは保証ではなく推測。libvpl 仕様 (`mfxstructures.h` の `mfxFrameInfo.FilmGrain` 説明) では AV1 デコードで `FilmGrain != 0` のとき `MORE_SURFACE` が返る可能性が明記されている。

### Encoder 側との対比

`src/encode.rs:549` に定数、`src/encode.rs:1174-1198` の `encode_frame_async` で明示的にリトライ回数を制限:

```rust
const DEVICE_BUSY_MAX_RETRIES: u32 = 30;

for _ in 0..DEVICE_BUSY_MAX_RETRIES {
    let status = self.session.lib().mfx_video_encode_frame_async(...);
    if status == sys::mfxStatus_MFX_WRN_DEVICE_BUSY {
        std::thread::sleep(std::time::Duration::from_millis(1));
        continue;
    }
    // ...
}
Err(Error::new_custom(
    "MFXVideoENCODE_EncodeFrameAsync",
    "device busy after max retries",
))
```

Decoder は同じ設計を採用すべき。なお Encoder 側の `encode_frame_async` は `MORE_SURFACE` を含む全ステータスを `status != MFX_ERR_NONE` で一括エラーにしており、Decoder の MORE_SURFACE 即エラー化と方針が一致する。

### SKILL.md の記述との齟齬

`skills/shiguredo-vpl/SKILL.md:226` は「`MFX_ERR_MORE_DATA` と `MFX_WRN_DEVICE_BUSY` は内部で吸収される。 `DEVICE_BUSY` は 1ms スリープで最大 30 回までリトライ (旧 10 回から拡張)。 30 回を超えると致命的エラー。」と説明している。この記述はエンコーダ限定と明示されていないため、規範文書上「Decoder も 30 回上限」と読める。実装が規範から乖離している。

## 設計方針

- **DEVICE_BUSY**: 以下 2 箇所に Encoder と同じ上限リトライ (`30 回`) を導入する。定数は `decode.rs` 内で `const DEVICE_BUSY_MAX_RETRIES: u32 = 30;` を宣言する。Encoder 側の同名定数と値は一致するが、結合度を下げるために意図的に複製する。
  - `decode_bitstream` の DEVICE_BUSY 分岐
  - `finish` の drain ループの DEVICE_BUSY 分岐
- **MORE_SURFACE**: `surface_work=NULL`（VPL 内部割り当て）では本来返らないとされているが、libvpl 仕様では AV1 の `FilmGrain != 0` 時に返る可能性が明記されている。ただし `surface_work=NULL` 時の挙動は確認できていない。上限付きリトライにすると「発生しないはずのコードパス」に複雑さを持ち込むため、`continue` を除去し**発生時は即エラー (`Error::from_mfx`) で返す**。
  - もし実運用で AV1 FilmGrain によって `MORE_SURFACE` が発生するケースが確認された場合は、後続 issue で上限付きリトライに切り替える。この場合、SKILL.md の「MORE_SURFACE は即エラー」記述も併せて更新する。
- **`finish` の drain ループ**: 現在 `MORE_SURFACE` の明示的チェックがなく `status < 0` で暗黙に捕捉されている（挙動は `Error::from_mfx` でエラー）。`decode_bitstream` との可読性の一貫性を保つため、`finish` にも `if status == sys::mfxStatus_MFX_ERR_MORE_SURFACE` の明示的分岐を追加する。挙動自体は変更されない。
- 上限超過時は `Error::new_custom("MFXVideoDECODE_DecodeFrameAsync", "device busy after max retries")` を返す（Encoder 側のメッセージ形式に合わせる）。

### 修正後のエラー復帰に関する注意

`Decoder::decode` は `QueueFrame` を Worker に送信した後で `decode_bitstream` を実行する。本修正により `decode_bitstream` が上限超過で `Err` を返した場合、送信済みの `QueueFrame` が Worker の `pending_values` に残留する。Drop 時は `Stop` で `MFX_ERR_ABORTED` として通知されるが、エラー後も Decoder を再利用する場合、FIFO 順序が破綻する可能性がある。この制限は修正前から存在する設計上の限界であり、本修正では対応しない。

## 完了条件

以下すべてを満たす。

1. `src/decode.rs` の `decode_bitstream` / `finish` で `MFX_WRN_DEVICE_BUSY` のリトライに上限 (30 回) が導入され、上限超過時は `Error::new_custom(...)` を返す。
2. `src/decode.rs` の `decode_bitstream` で `MFX_ERR_MORE_SURFACE` の `continue` を除去し、既存の `status < 0` 分岐経由で `Error::from_mfx` により即エラーを返す。
3. `src/decode.rs` の `finish` の drain ループに `MFX_ERR_MORE_SURFACE` の明示的分岐を追加し、`Error::from_mfx` で即エラーを返す。
4. `src/decode.rs` に `const DEVICE_BUSY_MAX_RETRIES: u32 = 30;` を宣言する。Encoder 側の同名定数と常に同じ値を維持すること（値変更時は両方を同時に更新する）。
5. `finish` が上限超過エラーで抜けた後、`Decoder` を Drop した際に `stop_worker` が Worker スレッドを正常に join できること（Worker の `Stop` ハンドラが未消費 `pending_values` を `MFX_ERR_ABORTED` として通知し、スレッドが正常終了する）。
6. `skills/shiguredo-vpl/SKILL.md:226` の DEVICE_BUSY / MORE_SURFACE の説明に Decoder 側の挙動を追記する（「DEVICE_BUSY は Encoder / Decoder ともに最大 30 回リトライ」「MORE_SURFACE は Encoder / Decoder ともに即エラー」と明記）。
7. `CHANGES.md` の `## develop` に `[FIX]` として本修正を追記する。

## 影響範囲

- `src/decode.rs`（`decode_bitstream`: DEVICE_BUSY リトライ上限追加、MORE_SURFACE 即エラー化。`finish`: DEVICE_BUSY リトライ上限追加、MORE_SURFACE 明示的分岐追加。`DEVICE_BUSY_MAX_RETRIES` 定数宣言）
- `skills/shiguredo-vpl/SKILL.md`（L226 の DEVICE_BUSY / MORE_SURFACE 説明に Decoder 側の挙動を追記）
- `CHANGES.md`
