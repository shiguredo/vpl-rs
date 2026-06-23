# encode() でサーフェスがリークする

Created: 2026-05-01
Model: kimi-k2.6

## 問題

`Encoder::encode()` 内で `MFXMemory_GetSurfaceForEncode` で取得した内部サーフェスは、`encode_frame_async()` がエラーを返した場合に解放されない。

## 再現手順

1. `Encoder::encode()` を呼び出す
2. `MFXMemory_GetSurfaceForEncode` で内部サーフェスを取得する
3. `encode_frame_async()` で `MFX_ERR_DEVICE_FAILED` やデバイスビジーが 10 回連続で発生する
4. `?` により早期リターンするが、この時点で `mfx_frame_surface_release` が呼ばれていない

## 影響

サーフェスリソースがリークする。

## 解決方法

`SurfaceGuard` 構造体を導入して、取得した内部サーフェスを RAII で管理するようにした。`SurfaceGuard` は `Drop` で `mfxFrameSurfaceInterface::Release` を呼び出し、`encode_frame_async` などのエラーパスでも確実にサーフェスが解放される。成功時は `release()` で明示的に解放してガードを無効化する。
