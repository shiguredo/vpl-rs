# デコーダの非同期コールバック対応

Created: 2026-05-01
Model: deepseek-v4-pro

## 背景

エンコーダは既に非同期コールバック API（`Encoder<T>` + Worker スレッド + `EncodedFrame<T>`）に移行済みであるが、デコーダは従来の同期的な `decode()` → `next_frame()` パターンのままだった。デコーダもエンコーダと同様に非同期コールバックでデコード完了通知を受け取れるようにする必要がある。

## 要件

1. エンコーダと同様に、デコーダでも SyncOperation を行うワーカースレッドを用意し、そこからデコード完了のコールバックを呼ぶ
2. `AsyncDepth` の値を `DecoderConfig` で設定可能にする
3. デコード時に `value: T` を渡せるようにし、対応するフレームがデコードされたら `DecodedFrame` にその値を含める
4. `DecodedFrame` は現在 `data: Vec<u8>` を保持してサーフェースからコピーしているが、直接コールバックを呼ぶためこのコピーは不要になる。`y`, `uv` のスライスや `pitch` などを用意し、コールバック呼び出し中のみ寿命が保証されるデータを渡す

## 対応内容

- `Decoder<T>` をジェネリクス化し、コンストラクタでコールバックを受け取る
- `DecoderConfig` に `async_depth: Option<u16>` を追加
- `DecodedFrame<'a, T>` に変更し、`y: &'a [u8]`, `uv: &'a [u8]`, `pitch: usize`, `value: T` を提供
- `Decoder::decode` のシグネチャを `decode(&mut self, data: &[u8], value: T)` に変更
- `Decoder::next_frame` を削除
- `surface_work=NULL` の VPL 内部割り当て方式に移行し、自前のサーフェスプール管理を廃止
- Worker スレッド (`"vpl-decoder-sync"`) を追加し、エンコーダと同じ SyncOperation パターンを実装
- `finish()` / `Drop` もエンコーダと同じ `WaitIdle` / `Stop` パターンで実装

## 解決方法

Completed: 2026-05-01

`src/decode.rs` を全面書き換え、以下の変更を実施した。

### Decoder<T> のジェネリクス化と Worker スレッド追加

エンコーダの `encode.rs` と同様の非同期パターンをデコーダにも適用した。

- `Decoder<T>` 構造体に `worker_tx: mpsc::Sender<WorkerCommand<T>>`、`worker_handle: Option<thread::JoinHandle<()>>`、`pending_values: VecDeque<T>` を追加
- `Decoder::new(config, callback)` でコールバック `F: FnMut(Result<DecodedFrame<'_, T>, Error>) + Send + 'static` を受け取り、スレッド名 `"vpl-decoder-sync"` の Worker スレッドを起動
- `decode(&mut self, data: &[u8], value: T)` で `value` を `pending_values: VecDeque<T>` に蓄積
- サーフェス管理は `surface_work=NULL` で `DecodeFrameAsync` を呼ぶ VPL 内部割り当て方式に移行し、自前の `surface_buffers` / `surfaces` / `Locked` チェックをすべて廃止
- Worker は `SyncOperation` → `Map(READ)` → スライス作成 → `callback` → `Unmap` → `Release` の順で処理
- `finish()` は `WaitIdle` パターン、`Drop` は `Stop` → `join` パターンでエンコーダと一貫性を持たせた

### DecodedFrame<'a, T> への変更

- `data: Vec<u8>` を削除し、`y: &'a [u8]`、`uv: &'a [u8]`、`pitch: usize`、`value: T` に変更
- `width()`, `height()` は維持
- `value()`, `into_value()` アクセサを追加
- コールバック型に `for<'a> FnMut(Result<DecodedFrame<'a, T>, Error>)` (HRTB) を使用し、`y`/`uv` スライスがコールバック呼び出し中のみ有効であることを型レベルで保証

### DecoderConfig への async_depth 追加

- `async_depth: Option<u16>` フィールドを追加
- `None` の場合はデフォルト値 4（VPL 推奨の高スループット値）を使用

### 変更ファイル

- `src/decode.rs` — 全面書き換え（742 行）
- `src/lib.rs` — re-export とドキュメント例を更新
- `tests/test_roundtrip.rs` — コールバックパターンに適合するようテストを書き換え、デコーダ側の value 検証テストを追加
- `CHANGES.md` — `[CHANGE]` エントリを追記

### 設計上の判断

**VPL の内部バッファリングと pending_values**

VPL の `DecodeFrameAsync` は `MORE_DATA` 時にビットストリームデータを全消費して内部バッファに蓄積し、
十分なデータが溜まった時点でフレームを出力する。
このため `decode()` 呼び出しとフレーム出力は 1:1 に対応せず、value の管理には `pending_values: VecDeque<T>` の FIFO キューが必要だった。

value の push は `initialize()` 成功後に行うことで初期化エラー時の残留を防ぎ、
`decode_bitstream()` がエラーを返した場合は `pending_values.clear()` で状態をリセットしている。

**WorkerCommand::DrainFrame**

value が枯渇した状態でフレームが出力された場合や、`finish()` のドレイン時に value がない場合は
`DrainFrame` コマンドを送信する。Worker は `SyncOperation` + `Map` + `Unmap` + `Release` のみを行い、
コールバックは呼ばない。

`finish()` のドレインループは `decode_one_drain_frame` を別関数に分離せず `finish()` 内に直接記述し、
コードの見通しを良くした。

**DEVICE_BUSY の再試行**

`decode_bitstream()` では `DEVICE_BUSY` 時に 1ms スリープして再試行する。
エンコーダと異なりリトライ回数の上限は設けていない（`while bs.DataLength > 0` ループが自然に終了するため）。

**ビットストリームの連結禁止**

ストリーミング処理を前提とするため、テストであっても全フレームのビットストリームを連結して
1 回の `decode()` でまとめて処理することは禁止した。各フレームのビットストリームを個別に `decode()` に渡し、
`pending_values` により value の対応を取る方式でテストを実装した。
