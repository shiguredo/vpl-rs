# Decoder の DEVICE_BUSY / MORE_SURFACE リトライに上限がなく GPU 異常時にプロセスがハングする

- Priority: High
- Created: 2026-07-01
- Completed: {YYYY-MM-DD}
- Model: Opus 4.7
- Branch: feature/fix-decoder-device-busy-infinite-retry
- Polished: {YYYY-MM-DD}

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

`finish()` の drain ループも同じく上限なし。`Decoder::finish` は「コールバックが呼び出され終わるまでブロックする」契約（`src/decode.rs:405-407`）なので、BUSY が返り続けると `finish` 呼び出しが永久にブロックし、Drop から抜けることもできない。

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

Decoder は同じ設計を採用すべき。

### SKILL.md の記述との齟齬

`skills/shiguredo-vpl/SKILL.md:226` は「`MFX_ERR_MORE_DATA` と `MFX_WRN_DEVICE_BUSY` は内部で吸収される。 `DEVICE_BUSY` は 1ms スリープで最大 30 回までリトライ (旧 10 回から拡張)。 30 回を超えると致命的エラー。」と説明している。この記述はエンコーダ限定と明示されていないため、規範文書上「Decoder も 30 回上限」と読める。実装が規範から乖離している。

## 設計方針

- Encoder 側と共通の定数を使う。`DEVICE_BUSY_MAX_RETRIES` は `src/vpl.rs` などに公開して encode / decode 双方から参照する、あるいは `decode.rs` で同じ値の定数を宣言する（DRY を取るなら前者）。
- Decoder 側の 2 箇所（`decode_bitstream` / `finish`）に上限リトライを導入する。
- 上限超過時は `Error::new_custom("MFXVideoDECODE_DecodeFrameAsync", "device busy after max retries")` を返す。
- `MORE_SURFACE` にも同じ扱いで上限を掛ける。または「`surface_work=NULL` では `MORE_SURFACE` は発生しない」を仕様として明記し、発生時は即エラーで返す（防御的コードは削除）。
- 将来の改善として、Encoder / Decoder 双方で指数バックオフに切り替える案は別 issue に切り出す（本 issue は「無限ループを止める」のみに絞る）。

## 完了条件

以下すべてを満たす。

1. `src/decode.rs` の `decode_bitstream` / `finish` で `MFX_WRN_DEVICE_BUSY` のリトライに上限が導入され、上限超過時は明示的な `Error` を返す。
2. `MFX_ERR_MORE_SURFACE` の扱いを決め（即エラーまたは上限付きリトライ）、無条件 `continue` を無くす。
3. リトライ上限の定数は Encoder / Decoder で同じ値を使う（共有または個別宣言でも値は同じ）。
4. Decoder の Drop 経路が、上限超過エラーで抜けたあと問題なく Worker を join できることを確認する。
5. リトライ上限に到達したときに `Err` が返ることを検証する単体テストを追加する（`run_sync_worker` の周辺またはモック不要のシナリオで検証可能なもの）。
6. `CHANGES.md` の `## develop` に `[FIX]` として追記する。

## 影響範囲

- `src/decode.rs`（`decode_bitstream` / `finish` の 2 箇所のループ）
- 定数を共有する場合は `src/vpl.rs` または `src/encode.rs` の `DEVICE_BUSY_MAX_RETRIES` 参照
- `CHANGES.md`

## 参考

- `/review-code` の致命的指摘 F1
- Encoder 側の実装: `src/encode.rs:549, 1174-1198`
- `SKILL.md:226` の DEVICE_BUSY 説明
