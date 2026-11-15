# D5 / MB-12: 保存通知の振り分け

## 固定SDKで確認した根拠

Gradleが固定するAndroid Studio `2026.2.2.8`（build
`262.10968.63.2622.16730706`）の配布JARを`javap -p -c`で確認した。
以下はSDKの挙動の要約であり、実画面の受入結果ではない。

- `intellij.platform.ide.core.jar`の`Notification.notify` → `Notifications.Bus`は
  projectの通知topicへ発行する。
- `intellij.platform.ide.impl.jar`の`NotificationsManagerImpl.showNotification` /
  `showNotificationInner`はグループ設定に従いIDEのballoon / tool windowへ表示する。
  この経路に`SystemNotifications`への自動委譲はない。同配布物のplatform JARで
  `SystemNotifications`の参照元も確認し、汎用通知のlistenerからの委譲がないことを照合した。
- `SystemNotifications.getInstance()`はheadless / unit-test時にmuteを返す。
  `SystemNotificationsImpl.notify`は`SYSTEM_NOTIFICATIONS`がONかつ
  `Application.isActive == false`のときだけOS側へ委譲する。OS選択・利用不可時の処理はSDKが持つ。
- `NotificationsConfigurationImpl.areNotificationsEnabled()`は`SHOW_BALLOONS`を返す。
  OS通知フラグの公開getterはないため、固定SDKの`SYSTEM_NOTIFICATIONS`を読み取り専用で参照する。
  SDK更新時はこの参照と上記の自動委譲の有無を再確認する。
- `Notification.setSuppressShowingPopup(true)`は`NotificationsManagerImpl`で
  その通知だけの表示種別を`NONE`へ変える。グループ設定・履歴設定・通知のactionは変更しない。
  ただし履歴記録がOFFなら同managerが通知を即失効させるため、その設定ではpopupを抑止しない。

## 変更と重複防止

入口別やトレイのsnapshot更新からは通知しない。既存の共通保存通知
`ReplayProjectService.notifySave`で、次の条件がすべて成立するときだけSDKのOS通知へ振り分ける。

- メニューバー設定がON、IDEが非前面、projectが終了していない。
- IDEの通知全体・OS通知・グループの履歴記録がON、既存グループの表示が`NONE`ではない。
- 保存失敗、または`autoEventId`を持つクラッシュ自動保存の正常完了。

通常の保存成功・スクリーンショット・取消はOS通知にしない。OS側へ委譲した場合だけ
同じIDE通知のpopupを抑止し、通知自体は従来どおり発行する。履歴と「フォルダを開く」・
「パスをコピー」・失敗時の確認actionを残す。履歴記録OFFの場合も、既存actionへの到達を失わず
二重popupを避けるためOSには振り分けず、従来のIDE popupを残す。
SDK呼び出しが例外で失敗した場合もIDE popupを残す。
OS側の見出しにはproject名を含め、通知名には通知固有のIDを使ってproject間の衝突を避ける。

`ReplayProjectCapture.acceptSnapshot`が既にrequest ID・attempt・terminal phaseごとに通知し、
同じterminal snapshotの再発行や古いrevisionでは通知しない。再試行は新しいattemptとして扱う。
projectごとの経路と終了時の既存ガードを維持し、全projectをポーリングする別通知経路は設けない。

## 検証の限界

shellの回帰は、実SDKの設定と偽のOS通知先で振り分け・popup抑止・action維持を検証する。
OS通知の実表示・クリック・全画面や通知拒否／集中モード、macOS／Windows／Linuxの画面受入は別途必要。
SDKは配信成否を返さないため、OS側が表示しない場合でもIDEの履歴から確認する。
外部通知実装・dependency・OS分岐は追加していない。3OSのCI成功をこのshell検証だけで主張しない。
