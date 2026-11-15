# トリム・PNGの3OS出力一致 fixture

実端末の録画・ログを含まない合成入力です。`ReplayTrimTest` の
`rawSidecarIsRegeneratedForKeptSamples` と同じ H.264 生成方法を使い、
64×32 / 32×64 の2区間を64×64に正規化した24コマと元H.264を保存しています。
ログとタップには範囲内・範囲外・時刻不明の行を含めています。AACは含みません。

`ReplayTrimParityTest` はこの同一入力を200001〜1900001µsで切り取り、
先頭GOP再エンコード・後続コピー・side data・先頭コマPNGの出力SHA-256を
全OS共通の `output.sha256` と照合します。通常の `check` から呼ばれるため、
Linux `test` とmacOS / Windows `os-parity` で同じ比較が行われます。

生成IDとMP4の作成・更新日時（およびその日時由来のMP4ハッシュ）以外は除外しません。
改行、JSONの項目順、PNGの圧縮結果も比較対象です。入力のハッシュも検証し、
切り取り後に元ファイルが変わっていないことを確認します。
