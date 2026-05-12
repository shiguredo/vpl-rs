# 変更履歴

- UPDATE
  - 後方互換がある変更
- ADD
  - 後方互換がある追加
- CHANGE
  - 後方互換のない変更
- FIX
  - バグ修正

## develop

- [ADD] shiguredo_vpl::list_adapters と AdapterSelector / AdapterInfo / PciAddress / MediaAdapterType を追加する
  - @voluntas
- [CHANGE] EncoderConfig::new / DecoderConfig::new / codec_info::supported_codecs にアダプタ指定を必須化する
  - @voluntas

### misc


## 2026.1.2

**リリース日**: 2026-04-08

- [FIX] 正しいアライメントのバッファを VPL エンコーダーに渡すように修正
  - 以前はアライメントされていないサイズでバッファを計算していたため、バッファサイズが足りずに SIGSEGV が発生していた
  - @melpon


## 2026.1.1

**リリース日**: 2026-04-08

- [FIX] パック済みフォーマットでも Y/U/V に値を設定する
  - mfxFrameData のドキュメントに、NV12 や YUY2 のようなフォーマットの場合であっても、Y/U/V をそれぞれ設定する必要があると書かれているため
  - @melpon


## 2026.1.0

**リリース日**: 2026-03-31
