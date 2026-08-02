# Encoder が frame_seq=0 を TimeStamp として使うため一部 VPL ドライバで対応付けが破綻する可能性がある

- Priority: High
- Created: 2026-07-01
- Model: Opus 4.7
- Branch: feature/fix-encoder-frame-seq-zero-timestamp-collision
- Polished: 2026-08-02

## 目的

`Encoder::encode` の 1 フレーム目は `frame_seq = 0` を `mfxFrameSurface1.Data.TimeStamp` として渡す。VPL の一部ドライバ実装では TimeStamp = 0 を「未設定」として扱い、`bitstream.TimeStamp` に `MFX_TIMESTAMP_UNKNOWN` (0xFFFF_FFFF_FFFF_FFFF) や別の値を書き戻す挙動が報告されている。この場合 `pending_store.take_by_frame_seq(bitstream.TimeStamp)` が空振りし、1 フレーム目が「no pending frame for bitstream timestamp」でエラーになる。

現状のテストで顕在化していないが、別世代/別実装の GPU で当たる可能性がある。コード本体の修正コストは `frame_count` 初期値の 1 行変更のみであるため、予防的に対処する（加えてテスト・ドキュメントの更新が必要。詳細は「影響範囲」参照）。

## 優先度根拠

High。以下による。

- **1 フレーム目が必ず失敗する可能性**: もし該当ドライバに当たると、Encoder を起動して最初の入力が確実に失敗する。コード変更なしで発生する環境依存のため利用者側では回避不能となる。
- **対応方針が単純**: `frame_count` を 1 スタートにするだけで回避可能。修正コスト極小。
- **CI 検証がない**: 現在の実 GPU CI（`test-intel-vpl`）は特定のハードウェアで動くので、他ドライバでの再現性は未検証。

## 現状

### frame_seq = 0 の Data.TimeStamp

`src/encode.rs` の `Encoder::encode` は `frame_seq = self.frame_count` を取得し、`mfxFrameSurface1.Data.TimeStamp` に設定する。`Encoder::new` は `frame_count: 0` で初期化するため、最初の `encode()` 呼び出しでは `frame_seq = 0` が `Data.TimeStamp` に載る。

### take_by_frame_seq での引き当て

`src/encode.rs` の `sync_and_build_frame` は `synced.frame_seq`（= `bitstream.TimeStamp`）で `PendingFrameStore::take_by_frame_seq` を呼び、pending_store の登録キーと完全一致で引き当てる。もし VPL 側が TimeStamp を書き換えたら空振りする。

### VPL 側の TimeStamp 扱いの参考

- `mfxstructures.h` に `MFX_TIMESTAMP_UNKNOWN`（u64 表現で 0xFFFF_FFFF_FFFF_FFFF）が定義されている（`MFX_TIMESTAMP_UNKNOWN = -1`。`mfxdefs.h` には定義がない）
- `mfxFrameData.DataFlag` に `MFX_FRAMEDATA_TIMESTAMP_UNKNOWN = 0x0000`（「SDK がタイムスタンプを計算する」）と `MFX_FRAMEDATA_ORIGINAL_TIMESTAMP = 0x0001`（「オリジナルのタイムスタンプを pass-through する」）が定義されている。vpl-rs は `DataFlag` を設定していない（既定値 0 =「SDK が計算してよい」状態）
- ただし全ドライバが TimeStamp = 0 を「未設定」扱いするかは Intel の公式ドキュメントでは明確でない

### テスト状況

- `tests/test_roundtrip.rs` の全ラウンドトリップテストは 30fps × 8〜30 フレームで、frame_seq = 0 を含むが、テスト環境の GPU では 0 を有効な値として扱っているようで問題が顕在化していない。

## 設計方針

**frame_count を 1 スタートにする。**

- `Encoder::new` で `frame_count: 1` に初期化する（現在は 0）。
- `pending_store` の登録キーと `Data.TimeStamp` の両方が 1 以上になるため、VPL ドライバが TimeStamp = 0 を「未設定」扱いしても衝突しない。
- frame_seq は内部の連番で公開 API に直接露出しないが、`EncodedFrame::timestamp()` の値は `frame_seq * framerate_den` で計算されるため影響を受ける。修正後は最初のフレームの `timestamp()` が 0 ではなく `framerate_den` になる。この開始値の変化は後方互換のない変更の側面があるが、公開 API のシグネチャは不変であり、バグ修正の副作用として本 issue では `[FIX]` で扱い、`CHANGES.md` と `skills/shiguredo-vpl/SKILL.md` で開始値の変更を明示する（vpl-rs の「良い設計のためには破壊的変更を積極的に行う」方針に沿う）。
- `frame_count.checked_add(1)` のロジックは初期値に依存しないためそのまま利用できる。

### DataFlag の検討（本 issue の対象範囲外）

ドライバがタイムスタンプを計算・書き換えするかは `Data.TimeStamp` の値ではなく `mfxFrameData.DataFlag` で制御される仕様である。vpl-rs は `DataFlag` を設定していない（既定値 `MFX_FRAMEDATA_TIMESTAMP_UNKNOWN = 0x0000` =「SDK がタイムスタンプを計算してよい」）。

本 issue の frame_count 1 スタート化は「TimeStamp = 0 を未設定扱いする」**値起因の仮説**に対する予防的対応であり、**DataFlag 起因の仮説**（ドライバが `DataFlag = 0` を尊重してタイムスタンプを計算し直す）では修正が効かない可能性がある。`MFX_FRAMEDATA_ORIGINAL_TIMESTAMP` の設定は pass-through を要求する正攻法だが、vpl-rs はフレーム連番を 90kHz 単位の PTS として渡している（`mfxFrameData.TimeStamp` の仕様単位は 90kHz。既存の既知の不整合）ため、pass-through を有効にすると 90kHz 単位ではない値がそのまま伝搬される。DataFlag の設定とタイムスタンプ単位の是正は本 issue のスコープ外とし、タイムスタンプ設計の見直し（別 issue）と合わせて対応する。

### 代替案の不採用理由

- **`TimeStamp` と `frame_seq` を分離する案**: 実装変更が増えるわりに本質的な利点がないため不採用。
- **`Data.TimeStamp = frame_seq + 1` とする案**: 公開 API の `timestamp()` の開始値（0 スタート）を維持できる利点はあるが、`bitstream.TimeStamp` から frame_seq への逆算（-1）がマッチングのたびに必要になり、照合ロジックが複雑化する。vpl-rs の「良い設計のためには破壊的変更を積極的に行う」方針に照らし、frame_count 1 スタート化を採用する。

### ドレイン空フレームとの相互作用

`finish()` のドレインで生成されるビットストリームは `create_bitstream()` で `zeroed()` 初期化され `TimeStamp = 0` のままである。

- **修正前**: pending キーに 0 が存在するため、1 フレーム目の出力が未消費のままドレイン期に入った場合、空フレーム（`TimeStamp = 0`）が `take_by_frame_seq(0)` に一致してしまい、空フレームが 1 フレーム目の `user_data` を奪い、実フレームの出力が後から `Err` になる「サイレントな誤対応付け」が起こり得る。
- **修正後**: pending キーから 0 が確実に消えるため、ドレイン空フレームが `sync_and_collect` で `frame_seq = 0` として返されると `take_by_frame_seq(0)` が空振りして「no pending frame for bitstream timestamp 0」の `Err` 通知になる。これは誤一致が排除され常に明示的な `Err` になるという改善であり、この挙動は依存 issue 0008 が「0013 適用後」の前提としてテストヘルパーでの許容を計画している（詳細は「依存 issue」参照）。CI の GPU は空ドレインフレームを出さないため、既存テストの pass は妨げない。

## 完了条件

以下すべてを満たす。

1. `Encoder::new` で `frame_count: 1` に初期化する（現在は `frame_count: 0`）。
2. 既存の全ラウンドトリップテストが pass することを確認する。加えて、ラウンドトリップテストで最初の入力フレーム（`user_data == 0`）に対応する出力フレームの `EncodedFrame::timestamp()` が `framerate_den` と等しいことを assert する（開始値の変更を固定化し、将来のリグレッションを検出するため。`user_data == 0` のフレームを対象にすることで、gop_ref_dist = 1 の FIFO 出力順に依存しない）。なお、この assert は公開値 `timestamp()` の開始値を固定化するものであり、`mfxFrameSurface1.Data.TimeStamp`（公開 API に露出しない）の開始値そのものは検証できない。`frame_count: 1` 初期化の実装自体はコードレビューで確認する。
3. `skills/shiguredo-vpl/SKILL.md` の `EncodedFrame` の説明に、`timestamp()` の開始値（1 フレーム目 = `framerate_den`）を追記する。また、SKILL.md の「ドレイン時の空ビットストリームは空 `data` の `EncodedFrame` として正常通知される」の記述は、0013 適用後はドレイン空フレーム（`TimeStamp = 0`）が「no pending frame for bitstream timestamp 0」の `Err` 通知になり得る挙動と矛盾するため、その整合の扱い（依存 issue 0008 のテストヘルパーでの許容計画を参照し、エンコーダ側の対応は別 issue で行う旨）を SKILL.md に注記する。
4. `CHANGES.md` の `## develop` に `[FIX]` として追記する。`timestamp()` の開始値が 0 から `framerate_den` に変わることに言及する。

## 影響範囲

- `src/encode.rs`（`Encoder::new` の `frame_count` 初期化、1 行）
- `tests/test_roundtrip.rs`（1 フレーム目の `timestamp()` assert 追加）
- `skills/shiguredo-vpl/SKILL.md`（`timestamp()` の開始値の追記、ドレイン空フレーム整合の注記）
- `CHANGES.md`

## 依存 issue

- **issue 0008** (`0008-bug-decoder-b-frame-user-data-mismatch`): 0008 は本 issue を依存先として「Decoder 側の `frame_count` 初期値も 1 と整合させる」と記載している。また 0008 は「0013 適用後（`frame_count` が 1 スタート）はエンコーダのドレイン空フレーム（`TimeStamp = 0`）が pending に引き当てられず「no pending frame for bitstream timestamp 0」の `Err` 通知になる可能性がある」をテストヘルパーでの許容として計画している（「ドレイン空フレームとの相互作用」参照）。

## 参考

- `src/encode.rs` の `Encoder::new` の `frame_count: 0` 初期化（修正対象）
- `mfxstructures.h` に `MFX_TIMESTAMP_UNKNOWN`（u64 表現で 0xFFFF_FFFF_FFFF_FFFF）が定義されている
- `mfxstructures.h` に `MFX_FRAMEDATA_TIMESTAMP_UNKNOWN` / `MFX_FRAMEDATA_ORIGINAL_TIMESTAMP` が定義されている（`DataFlag`。本 issue のスコープ外）
