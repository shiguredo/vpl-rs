# VP9 HW エンコードが CI 環境で失敗する

Created: 2026-03-23
Model: Opus 4.6

## 概要

VP9 エンコーダーが CI の Intel GPU 上で `MFX_ERR_NULL_PTR` (-2) を返して失敗する。
`MFXVideoENCODE_Init` は成功するが、`MFXVideoENCODE_EncodeFrameAsync` でエラーになる。

## 再現手順

1. VP9 Profile0 / NV12 / CBR or CQP でエンコーダーを初期化する
2. フレームをエンコードする
3. `MFXVideoENCODE_EncodeFrameAsync` が `MFX_ERR_NULL_PTR` を返す

## エラー内容

```
Error { function: "MFXVideoENCODE_EncodeFrameAsync", status_code: Some(-2), status_name: Some("MFX_ERR_NULL_PTR") }
```

## 根拠

CI 環境の Intel GPU が VP9 HW エンコードに対応していない可能性がある。
GPU の世代やドライバーバージョンの確認が必要。

## pending の理由

CI 環境の GPU 仕様の確認が必要。VP9 HW エンコード対応の GPU が用意できるまで保留する。
