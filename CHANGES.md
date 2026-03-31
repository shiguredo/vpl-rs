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

- [ADD] supported_codecs() を追加する
  - @voluntas
- [CHANGE] 入力フレームフォーマットを NV12 / YUY2 / BGRA に変更する
  - I420, YV12, P010 を削除し、YUY2 を追加する
  - @voluntas
- [CHANGE] VP9 ラウンドトリップテストを一時的に削除する
  - CI 環境の Intel GPU での VP9 HW エンコード対応を確認するまで保留する
  - @voluntas
