"""Preserve exact 818b7e1, 210ed047, 945c9e0, 4b17fff, e22cac5, 30c62bb and 4d0430d contracts for historical unit tests.

This is not a generator or current task plan. Keep original wording and hashes;
current performance acceptance follows docs/verification/responsibility.md.
Synthetic test results never establish product acceptance.
"""

CASES = {'docs/verification/changes/issue-287.json': [],
 'docs/verification/changes/issue-206.json': [{'artifact': 'plugin',
                                               'preconditions': 'cleanな固定source SHA、標準buildの同一ZIP/全JAR '
                                                                'hash、identity.propertiesのsource/dirty/versionと実ロード版を照合する。共有GUI '
                                                                'leaseと専用IDE/project/Emulator '
                                                                'fixture、元設定・所有資源を保全。#224の定量測定と重ねない。既存のQA '
                                                                '#65のREAL/人間延期、原Case・原観察・失敗を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/206',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/280',
                                                              'docs/verification/responsibility.md: trusted '
                                                              'mainの有効保証・最小検証方法。既存Caseの全有効契約と延期を保持する。'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '新しい固定候補での実ロード・native/端末観察は未実施。JVM回帰・build/構造検査とは区別する。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のQA '
                                                                   '#65の人間/REAL延期を保持し、本変更で新しい一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'next_action': '固定HEAD/baseの別session独立レビュー後、PMが第2版の固定候補で必要なnative/FILE観察を割り当てる。独立して残る試験は既存又は対応QA、main反映は '
                                                              '#280 へ双方向readbackして移管する。writer単独のbuild成功で完了にしない。',
                                               'id': 'STACK-LOAD',
                                               'change': 'Kotlin/Gradle/JDK/JUnitと同梱依存を最新stableへ更新し、JVM/API target '
                                                         '25を保つ。正式pluginVersionはbuild時に固定し、同じZIPを受入・公開へ渡す。',
                                               'steps': ['標準buildで依存hash・ZIP構造・source/dirty/version・Plugin '
                                                         'descriptorを照合。正式版の不正syntaxとdirty指定の拒否、同sourceで版を変更した際のidentity再生成を確認する。',
                                                         '専用profileへ同じ固定ZIPを導入し、実IDE/full '
                                                         'build/JBR/全JAR、About/action・ToolWindow・設定入口、取得OFFを確認。通常終了/再起動後も同じ版と設定を確認する。'],
                                               'expected': '対象Rabbit/JBR25でロードし、指定したsource/versionとZIP内容が一致する。旧class/JAR混在・API '
                                                           'linkage error・実行時の追加ツール取得がない。正式版を公開時に書き換えない。',
                                               'recheck': 'compiler/toolchain/SDK・依存・版/identity処理の変更時に影響するbuild/ロードを確認し、異なるZIPへ旧観察を移植しない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': 'cleanな固定source SHA、標準buildの同一ZIP/全JAR '
                                                                'hash、identity.propertiesのsource/dirty/versionと実ロード版を照合する。共有GUI '
                                                                'leaseと専用IDE/project/Emulator '
                                                                'fixture、元設定・所有資源を保全。#224の定量測定と重ねない。既存のQA '
                                                                '#65のREAL/人間延期、原Case・原観察・失敗を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/206',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/280',
                                                              'docs/verification/responsibility.md: trusted '
                                                              'mainの有効保証・最小検証方法。既存Caseの全有効契約と延期を保持する。'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '新しい固定候補での実ロード・native/端末観察は未実施。JVM回帰・build/構造検査とは区別する。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のQA '
                                                                   '#65の人間/REAL延期を保持し、本変更で新しい一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'next_action': '固定HEAD/baseの別session独立レビュー後、PMが第2版の固定候補で必要なnative/FILE観察を割り当てる。独立して残る試験は既存又は対応QA、main反映は '
                                                              '#280 へ双方向readbackして移管する。writer単独のbuild成功で完了にしない。',
                                               'id': 'SETTINGS-PERSISTENCE',
                                               'change': 'SerializablePersistentStateComponentのimmutable '
                                                         'state/atomic更新と変更追跡、service所有CoroutineScopeでの保存先検証を採用する。既存workspace '
                                                         'XMLのcomponent/fieldを維持。',
                                               'steps': ['旧XML形式で保持時間・保存先・自動/手動package・enabledを持つ使い捨てprojectを読み込み、設定値と実取得状態を照合する。設定を有効値へApplyし、未適用編集のCancel/reopenと通常再起動による復元を確認する。',
                                                         '保持時間/package/保存先の代表不正入力を拒否し旧設定を維持する。入力を連続変更した場合の最新結果、確認中Apply拒否、画面を閉じた後の取消し・再open、取得toggleとApplyの併存を確認。狭幅の案内・各fieldのaccessible '
                                                         'name/Helpを保持する。'],
                                               'expected': '既存設定を失わず、Applyとtoggleの更新を互いに上書きしない。旧snapshotは不変で変更追跡が進む。検証はEDTをfilesystem読取りで塞がず、破棄後/旧入力の結果を画面へ反映しない。入力エラーでpartial '
                                                           'applyしない。',
                                               'recheck': '保存形式・settings/service/Configurable・SDKの変更時に旧XML互換・atomic更新・dispose/cancellationと必要なnative接続を確認する。'},
                                              {'artifact': 'plugin',
                                               'preconditions': 'cleanな固定source SHA、標準buildの同一ZIP/全JAR '
                                                                'hash、identity.propertiesのsource/dirty/versionと実ロード版を照合する。共有GUI '
                                                                'leaseと専用IDE/project/Emulator '
                                                                'fixture、元設定・所有資源を保全。#224の定量測定と重ねない。既存のQA '
                                                                '#65のREAL/人間延期、原Case・原観察・失敗を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/206',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/280',
                                                              'docs/verification/responsibility.md: trusted '
                                                              'mainの有効保証・最小検証方法。既存Caseの全有効契約と延期を保持する。'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '新しい固定候補での実ロード・native/端末観察は未実施。JVM回帰・build/構造検査とは区別する。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のQA '
                                                                   '#65の人間/REAL延期を保持し、本変更で新しい一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'next_action': '固定HEAD/baseの別session独立レビュー後、PMが第2版の固定候補で必要なnative/FILE観察を割り当てる。独立して残る試験は既存又は対応QA、main反映は '
                                                              '#280 へ双方向readbackして移管する。writer単独のbuild成功で完了にしない。',
                                               'id': 'CAPTURE-DEPENDENCIES',
                                               'change': '同梱scrcpy server 5.0.1とR8 9.5.26で作った時計DEXへ更新。公開SystemClock '
                                                         'APIを直接呼び、旧4.0の中断取得journalも所有一致時に回収する。',
                                               'steps': ['専用Emulatorとmarker '
                                                         'fixtureで新server/clockの実起動、動画と両ログ・clock取得、Save後の継続取得、OFF/通常終了の所有cleanupを観察する。旧4.0 '
                                                         'journalは合成所有fixture回帰で既知停止ownerの限定回収と未知/生存/置換対象の保全を確認する。',
                                                         '新しい固定成果物で全FILEの元PTS・config/keyframe/session・MP4 '
                                                         'frame/時刻/可変duration/edit/preroll、両JSONLとclock/identityを既存検証器で照合する。正常同期と境界の有効精度/窓/欠落/保全契約を満たす最小の観察を選び、consumerの代表再生/open/copyは既存適格証拠と変更影響を個別評価する。'],
                                               'expected': 'wire/nonce/入力上限・clock読取順とmin API '
                                                           '24契約を維持し、固定server/clockが起動する。データ・時刻・切出し・可読性・復旧の既存有効保証を満たす。未知/未達/REALを成功へ換えず、所有外の端末process・ファイル・保存済み成果物を変更しない。',
                                               'recheck': 'server/DEX/R8/SDK入力・wire/clock/mux又は実行条件の変更時は影響する保証を新成果物で確認する。無変更保証の保持事実利用はtrustedな影響証明と独立評価に限る。'}],
 'docs/verification/changes/issue-224.json': [],
 'docs/verification/changes/issue-274.json': [{'artifact': 'plugin',
                                               'preconditions': '#274のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・source/dirty/versionを照合する。nativeはPMが割り当てた共有GUI '
                                                                'lease・所有IDE/project/端末fixtureと元設定の復元手順を用い、実ロードを同定する。unit/fake・FILE・native・性能を区別し、元Dの精度FAILとQA65のREAL/人間延期を保持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232180946',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232240754',
                                                              'docs/verification/responsibility.md',
                                                              'docs/plugin-design.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '固定した新候補のnative/FILE・性能観察は未実施。unit/fake/buildの結果はIssue/PRへ別記し実受入に換えない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新たな一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した取得・設定・保存・UIと共有clock/log/所有境界を新しい固定候補で確認する。旧観察を新buildへ自動転記しない。',
                                               'next_action': 'writerが固定HEAD/base/check/ZIPと最小native計画を渡して停止。既存の独立reviewerが評価し、PMが必要観察と#224の比較枠を割り当てる。実装Issueはこの段階でcloseせず、残試験とmain反映を対応QA/#280へ双方向readbackしてから移管する。',
                                               'id': 'LOG-ONLY-MODE',
                                               'change': '既定値を動画＋ログに保ち、OFF時だけログ専用へ変更し、設定・有効取得・固定保存のモードを区別する',
                                               'steps': ['同じ候補のXML/ProjectCapture/Core回帰で旧設定のdefault、LOG_ONLY保存復元、Applyのatomic拒否、ONとの競合を確認する。保持時間や対象アプリの既存反映と寿命を保つ。',
                                                         '所有projectの設定で動画＋ログの既定値、編集Cancel、OFFでログ専用Apply、ONを観察する。取得中の変更拒否で旧設定/sequence/失敗pinが残ること、反映中のON拒否、OFFで通常モードへ戻せることを確認する。',
                                                         '同じロード版の通常project再openで保存設定と有効モードを照合する。ToolWindowの対象外表示、固定保存モード、失敗カード、keyboard/accessible '
                                                         'name/狭幅を確認する。206 '
                                                         'SETTINGS-PERSISTENCE/STACK-LOADと観察を共有できるが結果を自動転記しない。'],
                                               'expected': '旧設定はVIDEO_AND_LOGS。LOG_ONLYはOFFで一式適用し、暗黙停止・未保存対象破棄なし。保存済みと稼働中が異なる場合は明示し、旧モードで新規ONしない。UI・設定復元・実取得モードが一致する。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#274のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・source/dirty/versionを照合する。nativeはPMが割り当てた共有GUI '
                                                                'lease・所有IDE/project/端末fixtureと元設定の復元手順を用い、実ロードを同定する。unit/fake・FILE・native・性能を区別し、元Dの精度FAILとQA65のREAL/人間延期を保持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232180946',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232240754',
                                                              'docs/verification/responsibility.md',
                                                              'docs/plugin-design.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '固定した新候補のnative/FILE・性能観察は未実施。unit/fake/buildの結果はIssue/PRへ別記し実受入に換えない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新たな一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した取得・設定・保存・UIと共有clock/log/所有境界を新しい固定候補で確認する。旧観察を新buildへ自動転記しない。',
                                               'next_action': 'writerが固定HEAD/base/check/ZIPと最小native計画を渡して停止。既存の独立reviewerが評価し、PMが必要観察と#224の比較枠を割り当てる。実装Issueはこの段階でcloseせず、残試験とmain反映を対応QA/#280へ双方向readbackしてから移管する。',
                                               'id': 'LOG-ONLY-CAPTURE',
                                               'change': '動画準備・端末server/encoder・forward/socket・動画ringを開始せず、時計と両ログを保つ',
                                               'steps': ['同候補のresource/store/fake回帰で動画resourceが使えない場合もclock準備が成立し、LOG_ONLYに動画push/server/forward/socket/GOPがないことを確認する。必要readerのheader未確認/不正入力を正常にしない。',
                                                         '同じ実ロード候補・所有端末のログ専用ONでclock/helper・main/all又はlegacy '
                                                         'all・対象識別を観察する。所有video '
                                                         'server/encoder/forwardとhost動画ringがないことを確認する。対象アプリの通常再起動・許可済みの1回の切断再接続で帰属/世代・窓/欠落を確認する。',
                                                         '同じfixtureの有限marker入力を固定してSaveする。device/appの全対象行のID・元payload・時刻/帰属・clockと窓をFILEで照合する。71 '
                                                         'LOG-REMAP、106 LOG-READER、206 '
                                                         'CAPTURE-DEPENDENCIESと影響する保証を対応づける。',
                                                         'OFF後に動画＋ログへ戻し、同じ候補で通常の動画取得と単一MP4・両ログSaveを代表観察する。全frame/PTS/窓/元データは既存FILE検査と共有し、Dの同期精度FAILや未測定性能の解消に換えない。'],
                                               'expected': 'ログ専用では動画経路が起動せず、必要な時計・全体ログと対象アプリログを維持する。通常モードで動画経路が復帰する。silent '
                                                           'drop、動画由来の架空gap、時計不明の既知化、旧FAILの付け替えがない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#274のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・source/dirty/versionを照合する。nativeはPMが割り当てた共有GUI '
                                                                'lease・所有IDE/project/端末fixtureと元設定の復元手順を用い、実ロードを同定する。unit/fake・FILE・native・性能を区別し、元Dの精度FAILとQA65のREAL/人間延期を保持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232180946',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232240754',
                                                              'docs/verification/responsibility.md',
                                                              'docs/plugin-design.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '固定した新候補のnative/FILE・性能観察は未実施。unit/fake/buildの結果はIssue/PRへ別記し実受入に換えない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新たな一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した取得・設定・保存・UIと共有clock/log/所有境界を新しい固定候補で確認する。旧観察を新buildへ自動転記しない。',
                                               'next_action': 'writerが固定HEAD/base/check/ZIPと最小native計画を渡して停止。既存の独立reviewerが評価し、PMが必要観察と#224の比較枠を割り当てる。実装Issueはこの段階でcloseせず、残試験とmain反映を対応QA/#280へ双方向readbackしてから移管する。',
                                               'id': 'LOG-ONLY-EMPTY',
                                               'change': '正常な両ログ0行を、未開始・取得失敗・対象/時計不明から区別してmetadata保存する',
                                               'steps': ['同候補のstore/fakeで未開始、必要readerの初回header未確認/矛盾、未確定アプリ、時計不明・旧世代、完全切断を与える。両0行の新規Save不可と理由を確認し、有効行が残る部分保存は保つ。',
                                                         '必要readerの正常旧tail・同一世代の時計・現在epochの対象が揃い正の窓を持つ静穏fixtureでSaveし、旧tailを出力しない。0行のmetadata保存・失敗pin/retry/反復Saveを同production '
                                                         'Writerで確認する。実端末で完全無出力を作るための追加操作は要求せず、合成fixtureの根拠とnativeの範囲を分ける。',
                                                         '固定production '
                                                         'Writerが生成した正常0行/ログ障害/動画＋ログの保存物を標準editorで辿る。schema '
                                                         '3のcapture_mode/coverage.requested/NOT_REQUESTED、README、clock/app履歴・gap・hashを照合し、135 '
                                                         'LOG-ONLY-READMEの動画なし理由と通常欠落を区別する。'],
                                               'expected': '正常0行は窓/metadataを保存できてもcoverage秒数は0。video '
                                                           'file/tailはnull、parts/video missing ranges/clock '
                                                           'scopesは空、frames.jsonlは0行。意図的な動画なしをmissingKindsへ加えず、ログ/時計の実失敗とunknownは残る。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#274のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・source/dirty/versionを照合する。nativeはPMが割り当てた共有GUI '
                                                                'lease・所有IDE/project/端末fixtureと元設定の復元手順を用い、実ロードを同定する。unit/fake・FILE・native・性能を区別し、元Dの精度FAILとQA65のREAL/人間延期を保持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232180946',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232240754',
                                                              'docs/verification/responsibility.md',
                                                              'docs/plugin-design.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '固定した新候補のnative/FILE・性能観察は未実施。unit/fake/buildの結果はIssue/PRへ別記し実受入に換えない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新たな一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した取得・設定・保存・UIと共有clock/log/所有境界を新しい固定候補で確認する。旧観察を新buildへ自動転記しない。',
                                               'next_action': 'writerが固定HEAD/base/check/ZIPと最小native計画を渡して停止。既存の独立reviewerが評価し、PMが必要観察と#224の比較枠を割り当てる。実装Issueはこの段階でcloseせず、残試験とmain反映を対応QA/#280へ双方向readbackしてから移管する。',
                                               'id': 'LOG-ONLY-SAVE-LIFETIME',
                                               'change': '既存の保持上限・単一writer/pin・retry/discard・失敗試行保全と所有終了をログ専用へ適用する',
                                               'steps': ['同候補のunit/fakeでログbyte上限・時間prune、旧世代拒否、同時Save拒否、保存先不在・固定retry・discard・OFF/closeを確認する。pinのモード/窓/行IDは後続設定/取得で変わらず、完了後に次要求を受け付ける。',
                                                         'PMが固定した有限の長時間/ログ量・反復Save計画で、取得ring/固定pin/完成物/失敗partialを別々に集計する。成功と失敗を含む要求数・メモリ・全disk量・owned '
                                                         'process/workerを確認する。既存の32MiBログ会計/最大1pinと、実RSS/全出力の増加を混同しない。',
                                                         '必要nativeは同じ所有fixtureで通常Save反復、代表する保存先失敗と同じ対象のretry又はdiscard、OFF/project '
                                                         'closeをまとめて観察する。41の保存窓/部分取得/寿命/固定保存/公開と197/198の保全/所有cleanupへ証拠を対応づける。失敗注入の全種をnativeで反復しない。'],
                                               'expected': '未処理要求や保護窓が無制限に増えず、保存済み/失敗試行/所有不明物を消さない。完成物はatomic非上書き。停止後に所有client/workerを残さず、未確認cleanupは保留として残す。完成物・保全partialの累積はring上限の達成へ換えない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#274のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・source/dirty/versionを照合する。nativeはPMが割り当てた共有GUI '
                                                                'lease・所有IDE/project/端末fixtureと元設定の復元手順を用い、実ロードを同定する。unit/fake・FILE・native・性能を区別し、元Dの精度FAILとQA65のREAL/人間延期を保持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232180946',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/274#issuecomment-6232240754',
                                                              'docs/verification/responsibility.md',
                                                              'docs/plugin-design.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '固定した新候補のnative/FILE・性能観察は未実施。unit/fake/buildの結果はIssue/PRへ別記し実受入に換えない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新たな一律human必須・免除・延期は作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した取得・設定・保存・UIと共有clock/log/所有境界を新しい固定候補で確認する。旧観察を新buildへ自動転記しない。',
                                               'next_action': 'writerが固定HEAD/base/check/ZIPと最小native計画を渡して停止。既存の独立reviewerが評価し、PMが必要観察と#224の比較枠を割り当てる。実装Issueはこの段階でcloseせず、残試験とmain反映を対応QA/#280へ双方向readbackしてから移管する。',
                                               'id': 'LOG-ONLY-LOAD',
                                               'change': '同じ端末・アプリ・入力条件の動画＋ログとログ専用を比較し、Android負荷とhost負荷を分ける',
                                               'steps': ['docs/verification/performance.mdの#224担当と、同一candidate/ZIP/実ロード・端末/IDE・入力・保持/Save・温度/容量・観測手段、回数/時間/停止/復元条件を比較前に固定する。ARB-log相当は実装されたLOG_ONLYを用い、動画取得後の削除で代用しない。',
                                                         '探索pilotは効果判定と分ける。Android側のCPU/メモリ/encoder/転送とhost側のCPU/RSS/ディスク/保存待ち、観測器負荷・未測定を別記し、同条件の有限な対比較を行う。実機とEmulator '
                                                         'host値を混同しない。',
                                                         'ログ/時計/窓/欠落/保全の品質を同時に照合する。Studio-logcatとの第2版目標、#282/#283最適化、Dの精度未受入は別の残条件として保持し、根拠のない改善率や全性能達成を主張しない。'],
                                               'expected': '動画停止の事実と実測負荷の差を別に示す。未測定・条件不足・不利な結果を残し、同等性/改善を推測しない。比較中も両ログ・時計・保存/所有保証を満たす。'}],
 'docs/verification/changes/issue-301.json': [],
 'docs/verification/changes/issue-284.json': [{'artifact': 'plugin',
                                               'preconditions': '#284のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・version/source/dirtyと実ロードを同定する。native操作は別割当の共有GUI '
                                                                'leaseと所有fixture、元設定の復元手順を用いる。unit/fake・FILE・native・実測を区別する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/284#issuecomment-6232598191',
                                                              'docs/crash-autosave.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実装と決定的検査の結果は固定PRへ記録する。新候補のnative/FILE/寿命/負荷受入はこの割当で未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新しい一律免除や必須条件を作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した検出・適用・固定保存・結果通知の保証を同一候補で評価する。元FAIL/unknownとQA65/298のpendingを保持し、旧観察を自動転記しない。',
                                               'next_action': 'writerは固定HEAD/base/check/ZIP/未実施範囲を渡して停止。PMが別sessionの独立実装reviewと必要な最小native/FILEを割当てる。独立残試験のQA移管と#280へのmain反映移管を双方向readbackするまで実装Issueをcloseしない。',
                                               'id': 'CRASH-AUTO-DETECT',
                                               'change': '標準Java/Kotlin報告とtrusted platform eventを照合し、誤検出と不明帰属を保存しない',
                                               'steps': ['決定的検査で必須型/長さ/終端/UID/PID/process/epoch/receipt/poll '
                                                         'freshness、opaque MUTF-8、重複/曖昧/PID再利用と別対象trafficを確認する。元record '
                                                         'ID/raw/decode statusを保つ。',
                                                         '同じruntime/所有fixtureで実際の標準headerとam_crash/reporter帰属を照合する。各有効modeで1回の標準JVM '
                                                         'positiveを観察し、普通ERROR/handled/非終了custom '
                                                         'handler/別アプリ/正常終了を各1回確認する。公表するsubprocessだけ1例追加する。形式不一致はUNKNOWNを残す。'],
                                               'expected': '照合済み対象だけ1事象として扱う。別対象/reportersは対象会計を使わず元ログへ残る。native/ANR・不明形式/帰属は成功にしない。全Androidやログ無欠損は主張しない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#284のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・version/source/dirtyと実ロードを同定する。native操作は別割当の共有GUI '
                                                                'leaseと所有fixture、元設定の復元手順を用いる。unit/fake・FILE・native・実測を区別する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/284#issuecomment-6232598191',
                                                              'docs/crash-autosave.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実装と決定的検査の結果は固定PRへ記録する。新候補のnative/FILE/寿命/負荷受入はこの割当で未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新しい一律免除や必須条件を作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した検出・適用・固定保存・結果通知の保証を同一候補で評価する。元FAIL/unknownとQA65/298のpendingを保持し、旧観察を自動転記しない。',
                                               'next_action': 'writerは固定HEAD/base/check/ZIP/未実施範囲を渡して停止。PMが別sessionの独立実装reviewと必要な最小native/FILEを割当てる。独立残試験のQA移管と#280へのmain反映移管を双方向readbackするまで実装Issueをcloseしない。',
                                               'id': 'CRASH-AUTO-SAVE',
                                               'change': '1つの固定mapperと保持窓で単一pin/writerへ自動保存し、元データと報告時刻の限界を保存する',
                                               'steps': ['決定的検査で両mode、最終mapperによる境界変化、26秒でのdrain、5秒receipt境界、Java証拠の窓外、不足の上下限、既存pinとの競合とretryを確認する。保存先なし/時刻不明/窓外/実データなしで空pinを作らない。',
                                                         'DETECTの2保存を共有して、全ログ行の元bytes/ID/status/帰属/時刻、auto_triggerの不変事実と実窓、既存frame/PTS/gap/hashの全対象をFILE検査する。動画＋ログは動画があれば単一MP4、ログ専用は意図的な動画なし。代表的な読取りは既存の必要観察に合わせる。'],
                                               'expected': 'eligibility・出力行・triggerは同じ固定換算。再試行は同じ窓/triggerを保持する。platform報告を死亡時刻と呼ばず、先行証拠不在や直前区間不足を明記する。元FAILや同期不明を消さない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#284のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・version/source/dirtyと実ロードを同定する。native操作は別割当の共有GUI '
                                                                'leaseと所有fixture、元設定の復元手順を用いる。unit/fake・FILE・native・実測を区別する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/284#issuecomment-6232598191',
                                                              'docs/crash-autosave.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実装と決定的検査の結果は固定PRへ記録する。新候補のnative/FILE/寿命/負荷受入はこの割当で未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新しい一律免除や必須条件を作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した検出・適用・固定保存・結果通知の保証を同一候補で評価する。元FAIL/unknownとQA65/298のpendingを保持し、旧観察を自動転記しない。',
                                               'next_action': 'writerは固定HEAD/base/check/ZIP/未実施範囲を渡して停止。PMが別sessionの独立実装reviewと必要な最小native/FILEを割当てる。独立残試験のQA移管と#280へのmain反映移管を双方向readbackするまで実装Issueをcloseしない。',
                                               'id': 'CRASH-AUTO-LIFETIME',
                                               'change': '既定OFFのApply境界・設定復元とcallbackに依存しない保存結果を保持する',
                                               'steps': ['設定/ProjectCapture/Coreで旧XML '
                                                         'defaultfalse、Applyと取消/再設定、OFF中の設定保持、対象/世代/時計境界を検査する。実writerの公開前取消・検証済み完了後OFF/dispose・rename後検証失敗・終了待ちtimeout・retry/古いcallbackを順序制御して検査する。',
                                                         '同じロード候補で設定下書き/Apply/通常再open、待機/有効/上限停止・直近結果/残警告とkeyboard/狭幅を確認する。変更した接続で残る不足を明示し、保存競合又はOFF/project '
                                                         'closeの1つだけ代表観察する。QA298の同一保証/同一候補の観察を共有できる。'],
                                               'expected': '未適用設定や古い観測を使わない。writerの検証済み結果はoptional '
                                                           'callback以前に確定し、OFF/disposeで失わない。flag/timeoutだけで取消完了とせず、失敗pin/保全出力を維持する。UIは生stackを公開せず、閉じたprojectへ更新しない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#284のcleanな固定source SHA・標準ZIP/全JAR '
                                                                'hash・version/source/dirtyと実ロードを同定する。native操作は別割当の共有GUI '
                                                                'leaseと所有fixture、元設定の復元手順を用いる。unit/fake・FILE・native・実測を区別する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/284#issuecomment-6232598191',
                                                              'docs/crash-autosave.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実装と決定的検査の結果は固定PRへ記録する。新候補のnative/FILE/寿命/負荷受入はこの割当で未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存の人間/REAL延期を保持し、新しい一律免除や必須条件を作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '変更した検出・適用・固定保存・結果通知の保証を同一候補で評価する。元FAIL/unknownとQA65/298のpendingを保持し、旧観察を自動転記しない。',
                                               'next_action': 'writerは固定HEAD/base/check/ZIP/未実施範囲を渡して停止。PMが別sessionの独立実装reviewと必要な最小native/FILEを割当てる。独立残試験のQA移管と#280へのmain反映移管を双方向readbackするまで実装Issueをcloseしない。',
                                               'id': 'CRASH-AUTO-BOUNDS',
                                               'change': '検出・履歴・受付数を有界にし、長時間/負荷/累積出力の残検証を追跡する',
                                               'steps': ['16件/64KiB・256終端・8受付・30秒の各境界、別対象300組以上の無会計、上限停止と明示的再arm、手動保存維持を決定的に検査する。queue/自動retry/別readerが増えないことを確認する。',
                                                         '#224/QA298担当が既存の同一候補の有限長時間/負荷計画へ自動保存ON/OFF、成功/失敗試行、ring/pin/完成物/partial別の集計を組み込む。時間・入力量・回数・観測器・容量停止/復元条件を実行前に固定する。今回のsource検査をRSS/Android負荷の実測に換えない。'],
                                               'expected': '上限で停止し手動保存と所有cleanupを保つ。8受付やring上限を全diskのquotaと呼ばない。完成物/失敗出力を自動削除しない。未測定/不利な結果を残し、負荷改善や全v2受入を推測しない。'}],
 'docs/verification/changes/issue-283.json': [{'id': 'HOST-LOG-TAIL',
                                               'artifact': 'plugin',
                                               'change': 'live判定を通過したreaderのclock履歴コピーだけを省き、元のログ取得・保存内容とAndroid側の取得条件を維持する',
                                               'preconditions': '#301/#284等の実採用developへ同期した固定source/標準ZIP/全JARと実ロード版を識別する。共有host/GUI枠・所有fixture・有限の入力/時間/容量・停止/復元条件を事前に固定する。既存の有効な責任契約と元観察/失敗/延期を保持し、unit/fakeの成功をnativeへ転記しない。',
                                               'steps': ['固定baseline/candidateで同一Wire入力の内容・順序・時刻/unknown・所属・ID関係と割当量を比較する。起動tail、live後の時刻後退、reconnect、buffer分割/部分失敗はproduction回帰で精密に確認し、nativeで同じ故障行列を繰り返さない。',
                                                         '#224等の割当済み取得観察と共有し、実ロードした候補で選んだ固定mode・buffer群・対象アプリ・入力頻度/分母を記録する。保持済み観察と影響評価で証明できない実取得/保存の不足だけを確認し、保存窓全体のraw '
                                                         'payload・行数/順序・所属・source時刻/unknown/gapを照合する。',
                                                         '取得コマンド・buffer構成・周期・動画設定を変更していないことを固定差分で照合し、対応する比較条件でAndroid側非悪化とhost資源を評価する。未測定のIDE応答・端末/電力・保存/HW性能を推定せず、host競合や観察器の差を残す。'],
                                               'expected': '起動前tailを混入させず、live後の遅延/後退行を捨てない。reader再接続は新たにtail判定し、opaque '
                                                           'payload・各bufferの一意な取込み・所属・unknown/gap・部分障害・停止/所有・保存保護を維持する。同一入力の不要な履歴コピーによる割当を減らし、測定条件で裏付けられない速度やAndroid負荷の改善を主張しない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/283',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/283#issuecomment-6233756028',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/283#issuecomment-6233799440',
                                                              'docs/verification/performance.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'H1の有限Wire比較・回帰はIssue/PRに別記する。最終採用baseの固定artifactと実取得/Android側guardの不足事実は未確認。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。今回の変更から新たな一律human必須条件を追加せず、既存のREAL/人間延期を維持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': 'コピー省略が影響するログ取込みとhost割当を確認する。時計算法・動画codec・SaveWriter・UIは変更しないため、無関係な全player/editorの再試験を追加しない。',
                                               'next_action': 'writerが固定HEAD/base/tree、実比較条件/原結果、回帰結果と残native確認をPMへ渡す。別セッションの独立reviewと最終base上の通常checksを経て、#224等の共有観察で不足を埋める。H1だけで#283全体をcloseしない。'},
                                              {'id': 'HOST-VIDEO-SELECTION',
                                               'artifact': 'plugin',
                                               'change': '保存窓と直前GOPを選ぶ反復indexOfを一巡へ置換し、選択条件・順序・時刻対応・pinと保存内容を維持する',
                                               'preconditions': '固定baseline/candidateのsourceとJARを識別し、同じ有限driver・入力・時計履歴・測定回数を使う。候補測定は通常build後に1回だけ実行し、全工程14分の外部watchdogと容量制限を適用する。旧83f比較と最終採用baseを区別する。',
                                               'steps': ['既存Store/Save回帰に加え、切出し直前の連続フレームとgap境界、unknown行、固定窓外、pin/releaseを確認する。',
                                                         '5400/27000フレームでproduction '
                                                         'captureの内容digest、caller割当/CPU/wallと対象thread・区間のprofileを記録する。63フレームremuxと133フレームmixedは同じ実sourceを保存し、全frame/config/順序/時刻と出力のgeometry/色/paddingを照合する。合成unknown境界を元native観察へ読み替えない。',
                                                         '#284採用後の実baseでmanual/自動保存の呼出しと変更範囲を再確認し、影響する回帰と通常checksを行う。共有native受入では保持済み証拠で証明できない実保存の不足だけを確認する。'],
                                               'expected': '保存するframe/GOP、順序、時刻・unknown/gap、pin/release、出力内容を維持し、同じ一覧に対する反復indexOfを除く。競合条件下の単回比較から速度改善率・IDE応答・Android負荷・HW効果を断定しない。時計mapの割当とcodec処理は別の未変更箇所として扱う。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/283#issuecomment-6234387467',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/283#issuecomment-6243350923',
                                                              'docs/verification/performance.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '有限profileと回帰の実結果はIssueへ別記する。最終採用baseと実ロード版の不足確認は未完。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のREAL/人間延期を維持し、新たな一律human必須条件を追加しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '対象選択と保存内容・時刻/品質を確認する。時計算法・codec・取得コマンド・UIは変更せず、H3/cache/HW試作や全player/editor行列を追加しない。',
                                               'next_action': '固定候補と有限比較・原結果をPMへ渡し別セッションの独立reviewを受ける。#284採用後の実base上のchecksと残native受入を経るまで、この段階だけで#283をcloseしない。'}],
 'docs/verification/changes/issue-103.json': [{'artifact': 'plugin',
                                               'preconditions': '#103のclean固定source/ZIP/全JARと実ロードを同定。共有GUI '
                                                                'leaseと所有するIDE/project/端末fixture・保存先・復元担当をPMが割り当てる。現在の実装割当はGUI/ADB/device/install/restartを含まない。旧FAIL/unknown、human/REALと最終v2条件を保つ。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/103',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/103#issuecomment-6233411932',
                                                              'docs/cli.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '新しい固定候補のnative/FILE観察は未実施。決定的JVM/同梱検査は別に記録する。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のhuman/REAL延期を保持し、別の必須条件や免除を仮定しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': 'CLI入口・対象選択・設定移行・shared '
                                                          'service/writer結果・停止の変更を同じ候補で確認。既存Caseは元の結果を残し、未変更の事実だけを責任契約に従って評価する。',
                                               'next_action': '固定PRの独立レビュー後、PMが以下の最小native/FILEと残条件をQA/#280へ双方向readbackで割り当てる。実装の停止だけでIssue '
                                                              'close又は原型退役の受入済みとはしない。',
                                               'id': 'CLI-ENTRY-MIGRATION',
                                               'change': '同梱replay/replayd、旧command/configと出力/exit codeの明示移行',
                                               'steps': ['配布ZIPから展開した同じJARのCLIを使い、空白pathでhelpと引数不正を確認する。IDE未起動/未知project/複数接続/別JARは要求を送らず失敗することを決定的回帰と代表nativeで分けて確認する。',
                                                         '所有projectでOFF時に旧JSON又は簡易YAMLを明示移行し、保持時間/保存先だけが反映し、対象アプリ/モード/自動保存が保たれることをGUIで確認する。ON時や不正/重複/未知値は一式拒否、旧fileは不変。',
                                                         'replayd/daemonで同じ取得がONになり有限で戻ること、foreground警告/daemon拒否、statusの状態とGUI一致を確認する。旧独立daemon/launchdを混在させず、復旧はGit固定sourceを別の所有fixtureへ取り出して原blob/modeを照合する。'],
                                               'expected': '同じ配布物から利用でき、旧操作/設定/形式の差と復旧が明示される。勝手なIDE起動、対象選択、旧stateの変更、設定の無断上書きがない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#103のclean固定source/ZIP/全JARと実ロードを同定。共有GUI '
                                                                'leaseと所有するIDE/project/端末fixture・保存先・復元担当をPMが割り当てる。現在の実装割当はGUI/ADB/device/install/restartを含まない。旧FAIL/unknown、human/REALと最終v2条件を保つ。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/103',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/103#issuecomment-6233411932',
                                                              'docs/cli.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '新しい固定候補のnative/FILE観察は未実施。決定的JVM/同梱検査は別に記録する。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のhuman/REAL延期を保持し、別の必須条件や免除を仮定しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': 'CLI入口・対象選択・設定移行・shared '
                                                          'service/writer結果・停止の変更を同じ候補で確認。既存Caseは元の結果を残し、未変更の事実だけを責任契約に従って評価する。',
                                               'next_action': '固定PRの独立レビュー後、PMが以下の最小native/FILEと残条件をQA/#280へ双方向readbackで割り当てる。実装の停止だけでIssue '
                                                              'close又は原型退役の受入済みとはしない。',
                                               'id': 'CLI-SHARED-SAVE',
                                               'change': 'CLI保存とGUIが同じバッファ・single writer・確定結果を共有する',
                                               'steps': ['同じロード候補と有限marker入力で、VIDEO_AND_LOGSとLOG_ONLYのCLI保存を各1回。GUI操作とCLIのSaveを競合させ、writer/取得process/接続/バッファを重複生成せず既存のbusy/failed-pin制約になることを確認する。全raceの反復は決定的writer回帰へ委ねる。',
                                                         'CLIのexit0/path、GUIのrequest/attempt結果、完成sessionのsave_id/内容を照合する。2保存の全対象log '
                                                         'rows/bytes/ID/帰属/窓と、AVの全frame/PTS/窓/gap/hashを既存FILE手段で確認する。動画の代表再生と全文検証を区別する。',
                                                         '代表の保存失敗でexit1/stderr、未完成/retained path、同じpinのGUI '
                                                         'retry又はdiscardを確認する。早期writer完了、OFF/closeとの競合、同requestの別attempt、timeout非取消しは決定的回帰で確認し、その結果を実native実施へ読み替えない。'],
                                               'expected': '受理だけを成功にせず、要求したwriter '
                                                           'attemptの確定結果だけを返す。別要求のpathを返さず、不足/unknown/失敗は保持する。完成/失敗試行/所有不明物を消さない。'},
                                              {'artifact': 'plugin',
                                               'preconditions': '#103のclean固定source/ZIP/全JARと実ロードを同定。共有GUI '
                                                                'leaseと所有するIDE/project/端末fixture・保存先・復元担当をPMが割り当てる。現在の実装割当はGUI/ADB/device/install/restartを含まない。旧FAIL/unknown、human/REALと最終v2条件を保つ。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/103',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/103#issuecomment-6233411932',
                                                              'docs/cli.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '新しい固定候補のnative/FILE観察は未実施。決定的JVM/同梱検査は別に記録する。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のhuman/REAL延期を保持し、別の必須条件や免除を仮定しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': 'CLI入口・対象選択・設定移行・shared '
                                                          'service/writer結果・停止の変更を同じ候補で確認。既存Caseは元の結果を残し、未変更の事実だけを責任契約に従って評価する。',
                                               'next_action': '固定PRの独立レビュー後、PMが以下の最小native/FILEと残条件をQA/#280へ双方向readbackで割り当てる。実装の停止だけでIssue '
                                                              'close又は原型退役の受入済みとはしない。',
                                               'id': 'CLI-OWNERSHIP-LIFETIME',
                                               'change': '外部入力・接続断・停止・project closeの所有と結果不明を扱う',
                                               'steps': ['決定的transport回帰で不正UTF-8/長さ/command、相互認証失敗、symlink/所有権/permission不正、stale '
                                                         'PID開始時刻、複数/別build、4worker上限と接続断を確認する。ログや証拠へtoken/raw '
                                                         'diagnosticsを公開しない。',
                                                         '同じnative候補でCLI保存中のclient切断又は結果待ち終了を代表1回観察し、サーバー側の保存結果をGUIとFILEで照合する。自動retryせず結果不明を伝える。',
                                                         'stop/OFFとproject '
                                                         'closeをGUI/CLIで実行し、対象projectの所有取得だけが停止し完成物/保全partial/他project/IDE/旧daemonへ影響しないことを確認する。接続情報/worker終了を確認し、dispose後にUI更新しない。所有元設定を復元する。'],
                                               'expected': '認証前や対象不明の要求は操作しない。入力・worker・待機は有限。終了は所有対象に限定し、通信断を取消し又は保存失敗と誤認しない。'}],
 'docs/verification/changes/issue-315.json': [],
 'docs/verification/changes/issue-318.json': [{'id': 'CLI-IDENTITY-IDE',
                                               'artifact': 'plugin',
                                               'change': 'IDEで同定した実JARと同梱CLIが同じbuildで接続できる',
                                               'preconditions': '#318修正版を含む最終combined候補のsource/標準ZIP/全JAR/実ロード、共有GUI '
                                                                'leaseと所有projectをPMが固定する。今回writer枠ではnative未実施。',
                                               'steps': ['対象projectでdescriptorの生成・所有者・directory0700/file0600・PID開始時刻・実JAR '
                                                         'hashを確認する。',
                                                         '同梱CLIのstatusを1回実行してGUI状態と照合する。OFF/終了時の所有資源解放を既存CLI寿命契約に従って確認する。'],
                                               'expected': 'IDE '
                                                           'loaderでの同定失敗がなく、同じ配布JARのCLIだけが対象projectへ接続する。未知/別build/不正入力の拒否を保ち、所有外のprojectやIDEを変更しない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/318',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/318#issuecomment-6245251414',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/316',
                                                              'docs/verification/changes/issue-103.json'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'offline修復の確認と実native接続を分け、実ロード/descriptor/status/終了はQA316で未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '元human/REAL延期と全Case条件を保持し、新たな免除や合格を作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '固定修正を含む最終combined候補で、QA316の既存枠にreadiness確認を組み込む。',
                                               'next_action': 'PMがQA316へ固定成果物と残保証を双方向引継ぎし、readiness成立後に既存のCLI/FILE受入を続ける。#311最終候補と#224のCLI条件に共有する。'}],
 'docs/verification/changes/issue-311.json': [{'id': 'TOOLWINDOW-NARROW',
                                               'artifact': 'plugin',
                                               'change': 'ToolWindowのフォームと要約viewportを実表示幅へ追従させ、長文と操作の到達性を保つ',
                                               'preconditions': '修正版のcleanな固定source/標準ZIP/全JARと実ロードを同定する。共有GUI '
                                                                'leaseと復元条件を持つ既存QA308継続割当で行い、単独の追加GUI訪問は作らない。',
                                               'steps': ['長い保存先と自動保存の状態/残回数/警告/結果を表示し、広い表示から実狭幅へ変更する。IDE全体幅とToolWindow/要約の実表示領域を区別して記録する。',
                                                         '要約と保存先の全文末尾へ縦方向で到達でき、取得・設定・保存・必要時の再試行/破棄の操作とkeyboard経路を保持することを実画像と操作readbackで確認する。'],
                                               'expected': '長いlabel/pathが要約を表示領域の外へ押し広げず、文字や必要操作を右側で欠落させない。状態文の更新で読んでいるscroll位置を移動させず、元データ・設定・検出/保存/エラー内容とLifecycleを変えない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/311',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/308',
                                                              'docs/verification/changes/issue-284.json',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '固定1bef0cfの実ロード・狭幅画像/操作と実フォーム回帰、保持証拠評価は揃った。元NGだけの修復結果をrepairへ記録し、Case全体の正式独立確認・残条件はpendingのまま保持する。'},
                                               'human': {'status': 'pending', 'reason': '未観察。既存human/REAL条件を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '原284:CRASH-AUTO-LIFETIMEの失敗した狭幅保証だけを修正候補で確認する。#312の正式採用で原49e0と同tree '
                                                          'squash066565の出典を保持できる修正記録を用い、実観察・独立評価を統合前に結合する。',
                                               'next_action': '固定metadata '
                                                              'HEADと実行candidate1bef0cfを区別して通常必須検査・現feedback/providerを確認し、全contributorと異なる独立sessionが原本bundleと正式repair/whole '
                                                              'PRを検証する。QA308残consumer/BOUNDS22、QA316、human/REALとmain統合は既存ownerへ保持し、元NGを消さない。'}],
 'docs/verification/changes/issue-322.json': [],
 'docs/verification/changes/issue-324.json': [],
 'docs/verification/changes/issue-325.json': [{'id': 'SCREENSHOT-USB',
                                               'artifact': 'plugin',
                                               'change': '取得状態に依存せず選択アプリの現在画面を1クリックで完全保存する',
                                               'preconditions': 'PMが固定source/標準ZIP/実ロード、共有GUI/端末lease、廃棄可能なUSB端末の公開可能fixture画面と保存先を割当。製品の旧Caseと同期保証は別途保持する。',
                                               'steps': ['取得OFF/履歴なしで対象アプリを前面表示し、ToolWindowのボタンを1回押す。追加dialogなしの処理開始、EDT応答、成功通知、実PNGの元byte/全復号/画面内容と保存先を確認する。',
                                                         '同じ端末の既存取得中にもう1回保存し、既存取得状態・設定・未保存データを壊さず、別の完成フォルダになることを確認する。動画保存完了をスクショ開始の前提にしない。',
                                                         '対応する実端末の画面ID/focus/keyguard形式と非表示app/ロック時の説明を確認する。未接続/複数/切断/不正PNG/取消し/保存不可の回帰事実を照合し、native固有の不足だけを追加する。保護領域は空白になり得る説明を確認し、保護回避を要求しない。'],
                                               'expected': '1クリックで非同期取得が始まり、完全なPNGだけを非上書きで公開する。取得OFFでも有効で、取得ONの所有データを変えない。曖昧な端末/画面/appを推測で保存せず、画面全体・overlay・前後観察の非原子性・secure空白を明示する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/325',
                                                              'docs/requirements.md',
                                                              'docs/design/ide-ui.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerはsource/回帰を担当。実IDE/USBの割当と観察は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実際の操作感・画面の読みやすさは未観察。既存のhuman/REAL範囲を変更しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '最終候補で新しい撮影経路と既存取得への影響を確認する。無変更artifactの同じ証拠を目的なく再取得しない。',
                                               'next_action': 'PMが固定レビュー後に最小の実IDE/USB枠を割当し、残保証と実成果物を既存v2受入担当へ双方向引継ぎする。'},
                                              {'id': 'SCREENSHOT-ACTIONS',
                                               'artifact': 'plugin',
                                               'change': '通知からその保存場所を開き、その保存ファイルだけを削除する',
                                               'preconditions': 'SCREENSHOT-USBと同じ固定候補・lease・廃棄可能出力。2件のスクショと無関係な保持ファイルを区別できること。',
                                               'steps': ['成功通知の「保存場所を開く」でそのPNGを含むフォルダが実際に開くことを確認する。後のスクショ保存後も古い通知の対象が変わらないことを照合する。',
                                                         '「保存したファイルを削除」を押し、追加確認なしでそのPNGだけが消え、親フォルダ・他の保存・無関係なファイルは残ることを確認する。',
                                                         '改変/消失/置換/権限失敗/連続クリックの回帰と実通知の失敗説明を照合する。project終了時に進行中の撮影clientが停止し、遅延成功通知や旧通知による再起動がないことを目的に応じた最小方法で確認する。'],
                                               'expected': '通知は要求ごとの完成出力を保持する。OSで開けない/削除できない失敗を成功にせず、変更済み・置換済み・他人の出力を削除しない。UI応答・project寿命を保つ。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/325',
                                                              'docs/design/ide-ui.md',
                                                              'plugin/src/test/kotlin/io/github/shinma06/replaybuffer/core/ScreenshotSaveTest.kt'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実通知action→Finder/ファイルと終了のnative連携は未実施。'},
                                               'human': {'status': 'pending', 'reason': '通知の分かりやすさと操作感は未観察。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '同じ出力を使える既存観察とFILE回帰を再利用し、OS連携と実通知の不足を追加する。',
                                               'next_action': 'PMがSCREENSHOT-USBと同じ短い枠へまとめ、観察と残保証をv2受入担当へ渡す。'}],
 'docs/verification/changes/issue-328.json': [],
 'docs/verification/changes/issue-335.json': [{'id': 'RECORD-WARNING',
                                               'artifact': 'plugin',
                                               'change': '保存通知・ToolWindow・CLIで、記録不足と未確認の両方を含む上位警告を表示する',
                                               'preconditions': '#333と本変更を含み#104最終UIを反映した固定候補/標準ZIP/実ロードを同定する。既存の最終機能受入へまとめ、今回のunit成功や旧artifactの表示を新候補のnative '
                                                                'PASSへ換えない。',
                                               'steps': ['既存production回帰で時計対応不明でも全frameを保存するケース、実取得中断、保存失敗、正常静止VFR、LOG_ONLY/正常0行を照合し、missingKindsと詳細理由・JSON/API・保存結果の意味が変わらないことを確認する。',
                                                         '最終候補で通知・ToolWindowの不足/未確認の上位案内と詳細理由、同梱CLIの警告を読み戻す。時刻対応未確認だけを確認済みの映像欠落と断定せず、実不足と失敗の警告・再試行を保持し、正常静止画面と未要求動画を誤警告しないことを確認する。'],
                                               'expected': '未確認を欠落又は成功と断定せず、『記録の不足・未確認』として知らせる。実loss・reader失敗・時計/境界不明・保存失敗の判定と理由を残し、LOG_ONLY・正常0行・静止VFR・pin/原子保存・元PTSを変更しない。CLIのstdout保存先・exit '
                                                           'code・JSON構造を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/335',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/104',
                                                              'docs/verification/changes/issue-41.json',
                                                              'docs/verification/changes/issue-103.json',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'source/既存回帰と固定独立レビューを今回実施する。新文言の実ロード・表示は#104後の最終候補の受入まで未実施であり、unit結果をnative合格にしない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のhuman/REAL条件を保持し、今回新しい延期は追加しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': 'SYNC-01/UI-01/SAVE-03と41:SYNC-PARTIAL/CORE-SNAPSHOT/CORE-PUBLISH、103:CLI-SHARED-SAVEの説明に影響する。元Case/観察/FAILは保持し、変更していない取得・換算・writerの事実は現契約の独立した保証別影響評価を経て再利用する。',
                                               'next_action': 'PMが#104後の固定最終候補の既存受入へ本Caseと影響する表示確認を引継ぎ、現source/ZIP/ロードidentityと結果を別途記録する。main統合と全v2受入は#280で保持する。'}],
 'docs/verification/changes/issue-333.json': [{'id': 'SCREENSHOT-DEVICE',
                                               'artifact': 'plugin',
                                               'change': '録画と同じ端末を撮影し、OFFでは同じ解決規則を独立利用する',
                                               'preconditions': '#104後の同一固定候補/source/標準ZIP/実ロードを同定し、PM割当の既存#329/final '
                                                                'GUI枠・所有fixture・GUI/端末leaseを共有する。物理端末は原則利用不可であり、未確認を保持する。',
                                               'steps': ['source/fake回帰の対象A優先・A切断時の別Bへの非fallback・OFF単一候補/曖昧時拒否と要求snapshotの対応を照合する。保証を満たす同じ故障注入をnativeで反復しない。',
                                                         '利用可能な対応端末で、同じ固定候補の録画対象と撮影先を識別可能な画面で照合する。OFF・履歴なしでも撮影し、録画/ログ取得やbufferが開始されないこと、対象未確定の説明を確認する。Emulatorの画面診断形式が非対応なら、その拒否・未充足を記録し実USB成功へ読み替えない。',
                                                         '新たな成功PNGが得られれば既存SCREENSHOT-ACTIONSの通知/OS導線へ接続する。得られない場合も既存画像の正当な再利用条件と残保証を分け、新しい '
                                                         'per-feature USB枠や免除を自動追加しない。'],
                                               'expected': '取得中は録画対象serialを使い、ほかの端末への無断切替がない。OFFでは同じ単一候補規則で独立撮影し、取得を開始しない。対象/SDKの未確定や非対応は説明付きで拒否する。前面app・完全PNG・非上書き公開・保存ごとの通知/削除・project寿命を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/333',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/329',
                                                              'docs/requirements.md',
                                                              'docs/verification/changes/issue-325.json',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerはsource/自動回帰のみ。固定候補の実IDE/端末観察は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存human/REALの残条件と実USB未確認を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '既存#329と最終#104候補の共通枠で影響した対象選択・表示/統合だけを確認し、旧成功/失敗や未知を上書きしない。',
                                               'next_action': '独立固定reviewとdevelop統合後、PMが既存#329/#280へ残保証を双方向readbackして移管する。main/全v2/実機受入完了は別途。'}],
 'docs/verification/changes/issue-338.json': [{'id': 'SCREENSHOT-DESTINATION',
                                               'artifact': 'plugin',
                                               'change': '共通または専用の指定root直下へPNG1枚を保存する',
                                               'preconditions': '338→104採用後の同じ最終source/標準ZIP/実ロードを同定し、既存QA329の共有枠と所有fixtureを使う。新native/USB枠は未割当。物理端末は原則利用不可で固有未確認を保持する。',
                                               'steps': ['設定の共通既定と専用切替、非同期の入力エラー、Apply/Cancel、実際に表示される保存先を確認する。source/回帰で確認済みの全入力組合せをnativeで反復しない。',
                                                         '利用可能な対応画面で1クリック保存し、選択root直下のscreenshot-日時-UUID.png '
                                                         '1枚と追加dialog/包装なしを確認する。旧撮影フォルダや動画ログ出力は変わらない。PNGが得られなければ拒否/不足を記録し、旧画像を新撮影成功へ読み替えない。',
                                                         '同じ出力・既存SCREENSHOT-ACTIONSと共有して実通知の開く/削除を確認する。実画像の親が開き、その画像だけが消えて他画像/保存先は残る。file差替え/衝突/並行/取消しの回帰は既存証拠と照合し、未観察のOS統合だけを補う。'],
                                               'expected': '共通rootか明示専用rootへ、PNG1枚を非上書きで直接公開する。専用先不正時は共通へ無断切替しない。通知は実画像を保持し、所有画像のみ削除する。旧設定/CLI/動画ログ/端末固定/OFF独立/寿命を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/338',
                                                              'docs/requirements.md',
                                                              'docs/design/ide-ui.md',
                                                              'docs/verification/changes/issue-325.json',
                                                              'docs/verification/changes/issue-333.json',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerはsource/fake/filesystem/Swing回帰のみ。実IDE/通知/OSの観察は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実操作・表示は未観察。既存human/REALの延期を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '#104後の最終同一候補と既存329で影響した残保証のみを共有確認する。旧Case/観察/失敗を上書きせず、変更した保存形態は本Case/338で評価する。',
                                               'next_action': '固定review/develop採用後にPMが既存329へ残保証・手順/期待を、280へmain/全v2残件を双方向readbackして移管する。'}],
 'docs/verification/changes/issue-341.json': [{'id': 'SCREENSHOT-DESTINATION',
                                               'artifact': 'plugin',
                                               'change': 'Logcatのスクリーンショット設定（既定）または手動rootへPNG1枚を保存する',
                                               'preconditions': '341→104採用後の同じ最終source/標準ZIP/実ロードを同定し、既存QA329の共有枠と所有fixtureを使う。新native/USB枠は未割当。物理端末は原則利用不可で固有未確認を保持する。Logcatの画面撮影設定とログexport先を区別する。',
                                               'steps': ['Logcatのスクショ設定が参照され、設定画面の参照先表示と実際の保存rootが一致することを代表条件で確認する。Logcat設定を変更して次要求が追従すること、手動pathとApply/Cancel/不備の案内を確認する。設定初期値と空/相対の意味・取得不能時の案内は固定source/回帰の証拠と照合し、全入力をnativeで反復しない。',
                                                         '利用可能な対応画面で1クリック保存し、選択root直下のscreenshot-日時-UUID.png '
                                                         '1枚と追加dialog/包装なしを確認する。旧撮影フォルダや動画ログ出力は変わらない。PNGが得られなければ拒否/不足を記録し、旧画像を新撮影成功へ読み替えない。',
                                                         '同じ出力・既存SCREENSHOT-ACTIONSと共有して実通知の開く/削除を確認する。実画像の親が開き、その画像だけが消えて他画像/保存先は残る。file差替え/衝突/並行/取消しの回帰は既存証拠と照合し、未観察のOS統合だけを補う。'],
                                               'expected': '既定はLogcatの画面撮影設定を要求時に読み取って固定し、次要求は設定変更へ追従する。旧共通/キー欠落はこの既定へ移り、明示済み手動pathは保持する。手動でreplayと同じrootも受理する。取得不能/不正先では説明し、replay/Logcatの別rootへ無断切替しない。直下PNG1枚の非上書き公開・実画像の親を開く/所有画像のみ削除・端末一致/OFF独立/CLI/動画ログ/寿命を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/341',
                                                              'docs/requirements.md',
                                                              'docs/design/ide-ui.md',
                                                              'docs/verification/changes/issue-325.json',
                                                              'docs/verification/changes/issue-333.json',
                                                              'docs/verification/responsibility.md',
                                                              'docs/verification/changes/issue-338.json'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerは固定native class/所有Mock・fake '
                                                                 'adb/filesystem/Swing回帰のみ。実IDEの現在の設定値・OS '
                                                                 'Desktop・通知/Finderは未観察。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実操作・表示は未観察。既存human/REALの延期を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '#104後の同一最終候補と既存329で、Logcat設定から保存先への接続と影響した残保証だけを共有確認する。旧Case/観察/失敗を上書きせず、旧共通既定だけを本IssueのLogcat追従へ置き換える。',
                                               'next_action': '固定review/develop採用後にPMが既存329へ残保証・手順/期待を、280へmain/全v2残件を双方向readbackして移管する。'}],
 'docs/verification/changes/issue-104.json': [{'artifact': 'plugin',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/104',
                                                              'docs/design/ide-ui.md',
                                                              'docs/requirements.md',
                                                              'docs/verification/changes/issue-338.json',
                                                              'docs/verification/responsibility.md',
                                                              'docs/verification/changes/issue-341.json'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerはsource/自動回帰とheadless表示資料を担当。実IDE/操作の観察は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実際の分かりやすさ/読みやすさ・操作感は未観察。既存human/REAL延期を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'next_action': '固定review/develop採用後、PMが既存329/298/337と必要283の最終候補共有枠へ残保証を双方向引継ぎし、main/全v2は280/配布は271へ別途移管する。新QA/native枠を自動追加しない。',
                                               'id': 'GUI-FLOW',
                                               'change': '主要操作・対象/状態/保持窓と保存結果/失敗復旧を近接させる',
                                               'preconditions': '104採用後の最終固定source/ZIP/JAR/実ロードを同定し、PM割当の既存共有機能枠と所有fixtureを使う。原FILE/通常24入力/成功2保存を無条件に繰り返さない。',
                                               'steps': ['OFFで設定とスクショが独立利用でき、即時ON/OFFとApply/Cancelの違い、対象アプリ/Logcatスクショ保存先への追従または手動保存先（replayと同じpathも許可）が読み取れることを同じ設定観察で確認する。',
                                                         '既に必要な取得/保存/失敗/復旧の観察に合わせ、主要操作・現在の保持区間/LOG_ONLY/不足と未知・自動保存warningと保存完了/保全先を表示snapshot/実結果へ結び付ける。新しい故障を見た目のためだけに追加しない。',
                                                         '同じ失敗pinの理由・固定区間/アプリ履歴と再試行/別保存先/破棄の意味を確認する。完成時の開く/スクショの実画像親と所有画像削除は既存SCREENSHOT-ACTIONS/DESTINATIONと共有する。未完成を成功へ読み替えない。'],
                                               'expected': '頻繁な操作と対象・状態・保持窓が上部で分かり、保存結果と次操作を近くで確認できる。GUIの簡略化でgap/時計未知/失敗/固定pin/自動保存warningを隠さず、#335の不足・未確認、#338の直下PNGと#341のLogcat追従/手動保存先契約を維持する。',
                                               'recheck': '同一最終候補の既存329/298/337/必要283へ接続する。旧原観察を維持し、変更した表示/導線と現在の実ロード/統合だけを追加する。'},
                                              {'artifact': 'plugin',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/104',
                                                              'docs/design/ide-ui.md',
                                                              'docs/requirements.md',
                                                              'docs/verification/changes/issue-338.json',
                                                              'docs/verification/responsibility.md',
                                                              'docs/verification/changes/issue-341.json'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerはsource/自動回帰とheadless表示資料を担当。実IDE/操作の観察は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実際の分かりやすさ/読みやすさ・操作感は未観察。既存human/REAL延期を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'next_action': '固定review/develop採用後、PMが既存329/298/337と必要283の最終候補共有枠へ残保証を双方向引継ぎし、main/全v2は280/配布は271へ別途移管する。新QA/native枠を自動追加しない。',
                                               'id': 'GUI-ACCESS',
                                               'change': '狭幅・theme/DPI・keyboardと設定のラベル/説明を整える',
                                               'preconditions': 'GUI-FLOWと同じ最終候補/実IDE/fixture。元の操作手順やAX '
                                                                'index/寸法を固定値として流用せず、現在の内容を同定する。',
                                               'steps': ['狭いToolWindowと広幅を往復し、主要操作/保存復旧/長いパス・説明・不足詳細が横に切れず、縦scrollで全体へ到達することを必要な表示観察で確認する。',
                                                         '利用中のIDE '
                                                         'theme/表示倍率と標準keyboardで、操作名・label/選択状態・focus/Tab/Space・読取詳細の選択とコピーが使えることを確認する。自動回帰の標準LAF/拡大fontを実IDE切替の事実にしない。',
                                                         '同じ設定画面で長いpackage/Run情報/保存先エラーとApply/Cancelを確認し、取得ON/OFFを設定Cancelで巻き戻さないことを既存設定観察と共有する。 '
                                                         '設定画面の狭幅は実IDEが提供するviewport条件で評価し、ToolWindowの280幅を同一条件として押し付けない。'],
                                               'expected': '状態を色だけで伝えず、重要な操作と全文へ到達できる。標準focus/keyboardとlabel '
                                                           'associationを保ち、設定編集の反映時期と入力エラーが分かる。旧失敗・人間/REAL残件を合格へ換えない。',
                                               'recheck': '影響したlayout・ラベル・focus・IDE統合を最小の共有観察で確認し、既に十分な内容/FILE事実を鮮度だけの理由で再実行しない。'}],
 'docs/verification/changes/issue-347.json': [{'id': 'SAVE-WARNING-WINDOW',
                                               'artifact': 'plugin',
                                               'change': '保存範囲外と証明できる復旧履歴だけを不足・未確認の判定から除外し、窓開始の動画frameを保持する',
                                               'preconditions': '本変更と第2版最終UIを含む固定source/標準ZIP/実ロードを同定し、同ZIP製品classの限定回帰と既存337/343等の代表実IDE保存/表示を別証拠として共有する。精密fault注入のための新しい製品test '
                                                                'hookや全GUI matrixを追加しない。',
                                               'steps': ['同一gapのwindow内→誤差を含めwindow外への経過と再接続履歴をproduction '
                                                         'Store/Core/Writerの回帰で照合する。外側境界が証明できる場合だけ新規保存のwarning対象から外れ、動画の窓開始・元PTS/packet/config/履歴が保全されることを確認する。',
                                                         '本当に不明な時刻/境界、open/null、誤差範囲の交差、裏付け欠如/overflow/unknown boot '
                                                         'bridgeと窓内実lossの警告を保持する。実保存失敗後livewindowが進んでも元frozen対象のretryが元T/N/対象/bytes/理由を使い、新規saveとは区別されることを確認する。LOG_ONLYの未要求動画と正常app0行は警告理由にしない。',
                                                         '既存の最終候補の代表保存で、通知・詳細と同梱CLIがそのattemptの結果を表示し、正常保存/不足・未確認を伴う成功/保存失敗を区別する。READMEの履歴保全説明とsession.jsonの原gap/clockを確認する。精密な範囲内外は同じ固定ZIPのproduction '
                                                         'class証拠に結び付け、旧native観察を新実行としない。'],
                                               'expected': '証明可能な範囲外履歴が後の完全保存へwarningや動画先頭不足として残らない。重なりを否定できない不確実性と実不足は残り、元履歴/不明duration/時計/固定retry/失敗時保全を変更しない。#335の「不足・未確認」の用語、CLIのexit/stdout/JSON互換を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/347',
                                                              'docs/verification/changes/issue-41.json',
                                                              'docs/verification/changes/issue-71.json',
                                                              'docs/verification/changes/issue-335.json',
                                                              'docs/verification/responsibility.md',
                                                              'https://github.com/shinma06/android-replay-buffer/pull/350#discussion_r4357407923'],
                                               'gpt': {'status': 'pending',
                                                       'reason': 'writerがproduction回帰と通常checkを行うが、最終固定候補/実ロードの代表結合は未実施。自動成功だけでnative合格にしない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存のhuman/REAL条件と正式な延期を維持し、新しい延期や実行済みを追加しない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '41:CORE-SNAPSHOT/SYNC-PARTIAL/SYNC-WINDOW、71の保存窓・固定retry、52/85のclock '
                                                          'gap、274のLOG_ONLY、103の共有保存結果、335の表示へ影響する。原観察/FAILを保持し、影響部分/現在loadのfresh '
                                                          'factsと独立保証別影響評価を要する。',
                                               'next_action': 'PMが最終source/標準ZIPを固定し、同ZIPの精密production回帰と既存最終機能受入の代表表示へ結び付けてQAへ移管する。全v2/main受入は280で継続する。'}],
 'docs/verification/changes/issue-352.json': [],
 'docs/verification/changes/issue-345.json': []}


# Exact 210ed047 additions and changed fields; CASES keeps the 818b7e1 originals.
QUALITY_CASES = {'docs/verification/changes/issue-282.json': [{'artifact': 'plugin',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/282',
                                                              'docs/plugin-design.md',
                                                              'docs/verification/performance.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実IDEでの設定/適用・取得の観察は未実施。unit/fake/buildは別記しnative '
                                                                 'PASSにしない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '完成版の本人環境での比較と問題なし判断は未実施。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '影響する設定/品質引渡し・取得/保存と現在の実ロードを確認する。旧観察を新artifactの実行へ転記せず、自動性能比較を追加しない。',
                                               'next_action': 'writerが固定HEAD/base/check/artifactを04の独立レビューへ渡し、PMが必要な機能観察を既存QAへ接続する。性能は完成版本人比較の判断待ち、mainは#280で管理する。',
                                               'id': 'VIDEO-QUALITY-SETTINGS',
                                               'change': '既存設定で標準/軽量を選択し、明示適用・旧設定互換を保つ',
                                               'preconditions': '固定source/標準ZIP/全JAR・実ロードと所有fixtureを同定し、PM割当の機能枠/leaseで観察する。自動性能比較枠は割り当てない。',
                                               'steps': ['固定artifact/実ロードを同定し、既存設定で2選択肢の長辺/fps上限・指定bitrateと軽量tradeoff、取得OFF条件を確認する。',
                                                         'Apply/Cancelと保存/再ロード、旧XMLでの標準既定、LOG_ONLYの選択不可/値保持、取得ON/反映中の変更拒否を影響範囲で確認する。'],
                                               'expected': '既存の保持/保存先/アプリ等を保ち、勝手に軽量へ変えない。選択・現在適用値・反映待ちが区別できる。'},
                                              {'artifact': 'plugin',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/282',
                                                              'docs/plugin-design.md',
                                                              'docs/verification/performance.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実IDEでの設定/適用・取得の観察は未実施。unit/fake/buildは別記しnative '
                                                                 'PASSにしない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '完成版の本人環境での比較と問題なし判断は未実施。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '影響する設定/品質引渡し・取得/保存と現在の実ロードを確認する。旧観察を新artifactの実行へ転記せず、自動性能比較を追加しない。',
                                               'next_action': 'writerが固定HEAD/base/check/artifactを04の独立レビューへ渡し、PMが必要な機能観察を既存QAへ接続する。性能は完成版本人比較の判断待ち、mainは#280で管理する。',
                                               'id': 'VIDEO-QUALITY-CAPTURE',
                                               'change': '選択した取得条件を固定し保存metadataへ正しく残す',
                                               'preconditions': '固定source/標準ZIP/全JAR・実ロードと所有fixtureを同定し、PM割当の機能枠/leaseで観察する。自動性能比較枠は割り当てない。',
                                               'steps': ['同じ機能観察で必要な標準/軽量の取得を確認する。H.264/B-frameなし/IDR '
                                                         'interval指定/既存encoder選択を維持し、LOG_ONLYで動画を開始しない。実負荷の自動比較は行わない。',
                                                         '保存された要求quality/max_size/max_fps/video_bit_rateを取得時の設定と照合し、実寸法/PTS/内容とは分ける。既存の窓/時刻/gap/失敗pin・retryの証拠と影響回帰を使い、不足だけを確認する。'],
                                               'expected': '指定値が取得へ渡り、encode失敗で暗黙の解像度低下を行わない。受理済み保存の設定と内容を後の設定で付け替えず、既存保存/時計/ログ保証を維持する。'}]}

QUALITY_UPDATES = {'docs/verification/changes/issue-274.json': {'LOG-ONLY-MODE': {'next_action': 'PMが既存QA/最終候補で不足する機能観察だけを割り当て、性能は#224/#282の完成版本人比較・問題なし判断を待つ。自動対照比較枠を追加しない。残受入とmain反映は#280へ双方向readbackする。'},
                                              'LOG-ONLY-CAPTURE': {'next_action': 'PMが既存QA/最終候補で不足する機能観察だけを割り当て、性能は#224/#282の完成版本人比較・問題なし判断を待つ。自動対照比較枠を追加しない。残受入とmain反映は#280へ双方向readbackする。'},
                                              'LOG-ONLY-EMPTY': {'next_action': 'PMが既存QA/最終候補で不足する機能観察だけを割り当て、性能は#224/#282の完成版本人比較・問題なし判断を待つ。自動対照比較枠を追加しない。残受入とmain反映は#280へ双方向readbackする。'},
                                              'LOG-ONLY-SAVE-LIFETIME': {'next_action': 'PMが既存QA/最終候補で不足する機能観察だけを割り当て、性能は#224/#282の完成版本人比較・問題なし判断を待つ。自動対照比較枠を追加しない。残受入とmain反映は#280へ双方向readbackする。'},
                                              'LOG-ONLY-LOAD': {'gpt': {'status': 'pending',
                                                                        'reason': '旧の自動比較要求は終了。動画停止等の機能は既存事実・影響評価と不足観察を照合し、本人の完成版比較は代行しない。'},
                                                                'human': {'status': 'pending',
                                                                          'reason': '完成版を本人環境で比較し、問題なしという本人の明示判断待ち。現時点ではpending。'},
                                                                'next_action': 'PMが既存QA/最終候補で不足する機能観察だけを割り当て、性能は#224/#282の完成版本人比較・問題なし判断を待つ。自動対照比較枠を追加しない。残受入とmain反映は#280へ双方向readbackする。',
                                                                'change': 'ログ専用の動画経路停止を確認し、性能は完成版の本人環境比較・判断へ引き継ぐ',
                                                                'steps': ['動画OFF/LOG_ONLYでscrcpy/video '
                                                                          'socket/動画ringが始まらず、両ログ・時計・窓・欠落/保全が維持される機能事実を既存回帰と不足する機能観察で確認する。',
                                                                          '完成版のsource/配布artifactと本人環境で比較した品質選択・モード・利用条件・本人の実際の判断を記録する。現在は未実施であり、数値目標や自動比較を先行条件にしない。',
                                                                          '本人が問題なしと判断した場合に性能タスク完了可能とする。旧測定・不利結果・FAIL/unknownを保持し、この判断をAndroid≤Studioや実機全指標の数値証明へ換えない。'],
                                                                'expected': '動画を開始しない機能保証と、完成版の本人による性能受入を区別する。品質/内容/時計/窓/gap/保存・所有保証を維持し、本人判断前に性能PASSにしない。'}},
 'docs/verification/changes/issue-283.json': {'HOST-LOG-TAIL': {'steps': ['保持済みの固定baseline/candidateのWire比較と割当量の原結果を歴史上の証拠として参照し、再実行を要求しない。変更した内容・順序・時刻/unknown・所属・ID関係の不足だけを、起動tail・live後の時刻後退・reconnect・buffer分割/部分失敗の必要なproduction回帰で確認する。nativeで同じ故障行列を繰り返さない。',
                                                                          '#224等の割当済み取得観察と共有し、実ロードした候補で選んだ固定mode・buffer群・対象アプリ・入力頻度/分母を記録する。保持済み観察と影響評価で証明できない実取得/保存の不足だけを確認し、保存窓全体のraw '
                                                                          'payload・行数/順序・所属・source時刻/unknown/gapを照合する。',
                                                                          'H1当時の取得コマンド・buffer構成・周期・動画設定の不変事実を固定履歴で保持し、現在候補の#282品質選択とは分ける。Android/host数値非悪化の自動証明は要求せず、未測定を改善へ読み替えない。性能は完成版本人比較へ引き継ぐ。'],
                                                                'gpt': {'status': 'pending',
                                                                        'reason': 'H1の有限Wire比較・回帰と原観察は履歴として保持。現候補のログ内容/時計/所有・保存保証への適用と不足事実を評価し、追加の性能対照比較は要求しない。'},
                                                                'human': {'status': 'pending',
                                                                          'reason': '性能は完成版を本人環境で比較し、本人が問題なしと判断するまでpending。既存のREAL/人間延期と旧不利結果・FAIL/unknownは保持する。'},
                                                                'next_action': '固定候補・原観察/FAIL/unknown・必要回帰をPM/独立reviewへ渡す。既存証拠と影響評価で不足する機能/内容観察だけを既存QAで補い、性能は完成版の本人環境比較・問題なし判断待ち。自動対照比較・再profileを先行条件にしない。#280でmainを別管理する。'},
                                              'HOST-VIDEO-SELECTION': {'preconditions': '固定候補source/標準artifactと原baseline/候補の保持証拠を識別し、既存の内容・時刻・pin回帰と影響評価を使う。旧profileのdriver・回数・14分watchdogは過去の条件で、今回再実行の先行条件ではない。旧83f比較と最終採用baseを区別する。',
                                                                       'steps': ['既存Store/Save回帰に加え、切出し直前の連続フレームとgap境界、unknown行、固定窓外、pin/releaseを確認する。',
                                                                                 '旧5400/27000フレームprofileと63フレームremux/133フレームmixedの原結果を保持し、未測定の候補値へ付け替えない。変更の影響するframe/config/順序/時刻・geometry/色/paddingの不足だけを既存回帰/機能観察で照合する。追加profileや数値比較を完了条件にしない。',
                                                                                 '#284採用後の実baseでmanual/自動保存の呼出しと変更範囲を再確認し、影響する回帰と通常checksを行う。共有native受入では保持済み証拠で証明できない実保存の不足だけを確認する。'],
                                                                       'gpt': {'status': 'pending',
                                                                               'reason': '有限profile/回帰の原結果は保持。最終候補の保存内容・時刻/pin等の適用評価と必要な不足機能観察を扱い、性能は本人比較へ引き継ぐ。'},
                                                                       'human': {'status': 'pending',
                                                                                 'reason': '性能は完成版を本人環境で比較し、本人が問題なしと判断するまでpending。既存のREAL/人間延期と旧不利結果・FAIL/unknownは保持する。'},
                                                                       'next_action': '固定候補・原観察/FAIL/unknown・必要回帰をPM/独立reviewへ渡す。既存証拠と影響評価で不足する機能/内容観察だけを既存QAで補い、性能は完成版の本人環境比較・問題なし判断待ち。自動対照比較・再profileを先行条件にしない。#280でmainを別管理する。'}}}


# Exact 945c9e0 addition; historical fixtures above remain unchanged.
LOGCAT_CASES = {'docs/verification/changes/issue-369.json': [{'id': 'LOGCAT-PANEL-TOGGLE',
                                               'artifact': 'plugin',
                                               'change': 'IDE標準Logcatパネルを実表示状態に基づいて開閉する',
                                               'preconditions': '#369採用後の固定source/標準ZIP/全JARと実ロードを同定する。A6の復元・共有予約返却後、PMが既存QAの枠と所有projectを割り当てる。現在の旧候補の観察はそのidentityのまま保持する。',
                                               'steps': ['標準Logcatが利用可能なprojectでボタンから表示・非表示にし、IDE標準の実パネルと操作名/tooltipが一致することを確認する。IDE側で別に開閉した後も表示が追従し、次のボタン操作が実状態に対応する。',
                                                         '登録/利用不可・破棄後の拒否と遅延更新の寿命は回帰証拠を照合し、実IDEでは利用不可条件の無効表示/案内を不足分だけ確認する。同じ表示でTab/Spaceと幅の折返し・操作到達性を確認する。プラグインの取得ON/OFF・設定・保存とLogcatの既存tab/filterをこの操作が変更しないことを照合する。'],
                                               'expected': '独自Logcat '
                                                           'UIや取得processを追加せず標準パネルだけを開閉する。外部変更に追従し、未登録/利用不可/破棄後は無効化と理由表示、content/project破棄後は操作/予約更新を行わない。既存取得・保存・設定の契約を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/369',
                                                              'docs/design/ide-ui.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実IDEの開閉・外部状態変更・可読性/keyboardは未観察。模擬/回帰/buildは実IDEの受入とは別。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実操作・表示は未観察。既存human/REALの延期を変更せず、追加の本人判断gateを作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '新固定候補の実ロードと本Caseの影響範囲を確認し、旧source/ZIPの事実を新build実行へ転記しない。無影響の取得/保存/性能試験は反復せず、影響不明は不足として残す。',
                                               'next_action': '固定レビューとdevelop採用後、PMが実際のscopeに応じ既存QA329/343へ未完の独立試験を、280/271へ新候補/標準ZIPとmain反映を双方向readbackして移管する。writerはGUI/ADB/導入/再起動や正式ZIP '
                                                              'dispatchを行わない。'}]}


# Exact adopted 4b17fff addition; retain original pending status and repair boundaries.
SCREENSHOT_CASES = {'docs/verification/changes/issue-377.json': [{'id': 'SCREENSHOT-MIRROR',
                                               'artifact': 'plugin',
                                               'change': '録画のfocusなしmirrorがあっても標準物理画面の対象アプリを撮影できる',
                                               'preconditions': '元NGはIssue377の未編集acceptance-repairコメント6455508465と固定e091/f66 '
                                                                'packetで保持。PMが修正固定candidate/正式ZIP/JAR/実ロード・所有fixture・有限共有lease/返却を同定し既存QA329の枠へ割当。source担当はGUI操作しない。',
                                               'steps': ['原失敗の標準0＋focusなし仮想mirror、単一物理画面、選択serial/対象アプリ前面・点灯解除の条件を修正候補で対応付ける。',
                                                         '録画ON中に製品Screenshotを要求し、実PNGの対象画面/全復号/保存と既存取得の継続・設定/未保存データの保全を確認する。既存の適格なOFF/FILE/故障回帰と保証対応し、原NGに無関係な性能・全操作の反復を追加しない。',
                                                         '同じ成功PNG/通知を既存329の保存先・開く/所有画像削除の残保証へ共有できる場合は共有する。原FAIL/unknown/human/REALと旧/新artifact '
                                                         'identityを別に保持する。'],
                                               'expected': 'focusなし追加logical '
                                                           'displayだけで拒否せず、一意の物理画面の標準0に属する対象アプリを撮影する。ID2やscrcpy名に依存しない。曖昧な物理画面・focus/標準0不明・lock/未知形式は拒否し、serial固定、PNGとatomic非上書き、失敗保全と既存取得を維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/377#issuecomment-6455508465',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/329',
                                                              'docs/verification/changes/issue-325.json',
                                                              'docs/verification/changes/issue-333.json',
                                                              'docs/design/ide-ui.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '原NGの実ON撮影・完全PNG・同取得継続はrepair.GAP-1へ構成済み。手順FAILを保持し、最終metadata '
                                                                 'HEADの独立証拠評価・origin '
                                                                 'binding・必須checksは未完。Case全体のPASSではない。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存human/REAL・実機制約を保持する。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '原NGのON撮影と関連する実ロード/取得継続を新候補で再確認し、元失敗全保証のrepair結果と独立実質評価をdevelop統合前に固定する。',
                                               'next_action': 'BFが固定candidate f939の後に本Issue受入JSONだけをmetadata '
                                                              'commitする。04が最終HEAD/base/feedbackと原NG全保証の原本・帰属を独立評価してbindし、必須checks後にPMが統合判断。329/280/271の他残件・human/REALを保持。'}]}


# Exact adopted e22cac5 addition; model/source evidence does not establish native stream/render acceptance.
LOGCAT_TARGET_CASES = {'docs/verification/changes/issue-381.json': [{'id': 'LOGCAT-TARGET-DEVICE',
                                               'artifact': 'plugin',
                                               'change': '標準Logcatを開く際に現在のReplay '
                                                         'Buffer対象端末だけを選び、未準備・不明・破棄は明示する',
                                               'preconditions': '#381の固定source/標準ZIP/全JARと実ロード、確認済みStudio '
                                                                'buildを同定する。共有GUI枠と対象project/端末は既存PM/QAの割当に従う。旧4b/既存観察・原FAILと他の未完functional '
                                                                'unitは保持し、今回source '
                                                                'writerはGUI/ADB/導入/再起動/captureを行わない。',
                                               'steps': ['正規Replay対象Aと別端末Bを識別する。標準Logcatの選択中の単一端末タブをBにした後に閉じ、Replay '
                                                         'Bufferから開く。初回/復元/一覧準備/端末切替はIDE解決/初期化前の元クリックから固定された5秒の有限待機を確認する。復元offline '
                                                         'seedからonline/native接続へ元の期限内に到着すれば、再クリックせず表示されること、初めてonlineを確認した後の切断では要求を失効させること、同じボタンで待機を中止できることを確認する。全tabを閉じた空状態ではIDE側で端末用tabを開いてから再試行する案内を確認する。実端末セレクタがAを示し、実ログstream/表示がAのものでBのログが誤って表示されないことを区別して確認する。端末名やタブ名だけを根拠にしない。',
                                                         '標準側で手動の端末変更・タブ改名・セッション復元がある条件でも実所属とserialを確認する。分割tab・保存ログtab・対象なし/切断/未準備・非対応build/ABI・disposeでは該当する回帰証拠と不足する実表示を照合し、誤った端末を開かず理由を示すことを確認する。利用者のtab/filterを追加/削除/改名/強制変更しない。',
                                                         '同じ観察で閉じる操作、IDE側からの手動開閉同期、Tab/Spaceと理由の可読性を確認する。対象なしでも表示中Logcatを閉じられ、取得ON/OFF・Save・設定や他project/tabに変更を与えないことを確認する。未影響の保存/録画/性能試験は反復しない。'],
                                               'expected': '開く直前の正規serial/接続を再確認し、選択中の単一標準端末パネルの実モデルとnative接続端末が一致した場合だけ表示する。非同期切替のproperty一致を実stream/描画の証明とはせずnative観察を保持する。曖昧/保存ログ/不在/切断/未準備/非対応/破棄時は別端末へfallbackせず明示する。tab数とfilter、他tab/project、取得/Saveを維持する。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/381',
                                                              'docs/design/ide-ui.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '実SDK型のコンパイル/モデル/ボタン回帰と、実IDEの端末切替・stream/描画は別。新固定候補でのnative観察は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '実操作/表示は未観察。既存human/REAL延期は保持し、新しい本人承認gateは作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': '新固定候補の実ロードと端末選択の影響範囲を確認する。旧Case369/元候補の観察を新sourceの実行PASSへ転記しない。モデル選択・native接続・実stream/描画を区別し、切断後の再接続で古いOpenが復活しないことと、初期化後に5秒budgetを取り直さないことを確認する。待機中の対象/手動選択/tab/分割/表示/切断/dispose変更後に遅いOpenを行わないことを確認する。',
                                               'next_action': '固定sourceの独立09bレビューとdevelop採用後、PMが既存QA343だけへ本381の残る独立native試験、280/271へ固定候補/標準ZIPとmain反映を双方向readbackして移管する。QA329の従来責任を拡張しない。現在のsource担当にGUI/ADB/導入/再起動/正式ZIP '
                                                              'dispatchは割り当てない。'}]}


# Exact adopted 30c62bb addition; preserve all three steps and original pending actors.
ABOUT_SOURCE_CASES = {'docs/verification/changes/issue-387.json': [{'id': 'ABOUT-SOURCE-IDENTITY',
                                               'artifact': 'plugin',
                                               'change': '正式版descriptorを保持し、Aboutに同梱fullsource '
                                                         'SHAを併記して欠落/不正/読取失敗を明示する',
                                               'preconditions': 'Root割当の修正済みclean固定source/正式version0.2.0/標準ZIP/全3JAR/同梱identityと実ロードを同定する。元e22/aa97 '
                                                                'AboutのSHA未表示・原3/F8-01・既にqualifiedなartifact_binding/206load-restart/585事実と原FAIL/UNKNOWNを別identityで保持する。GUIは後段の既存共有枠/所有profile/fixtureで行い、本source/unit成功だけでは実表示/3行をPASSにしない。',
                                               'steps': ['About '
                                                         'actionを取得OFFの実ロード候補で開き、既存日本語説明・descriptor正式version0.2.0と、同じJARのidentity.sourceに一致する40桁source '
                                                         'SHAの初回表示/実action接続を確認する。descriptor/versionやZIPを書き換えず、旧e22 '
                                                         'SHAをhardcodeしない。',
                                                         '同梱sourceの欠落/短縮/不正文字/余分な文字とidentity読取失敗は小さいJVM回帰で明示unknown/reason・版と説明保持を確認する。例外の私的詳細や別値をSHAとして表示せず、CaptureResourcesのinstance/helper抽出・git/network/SDK/取得開始を実装に追加しない。実GUIで改変ZIPの新しい故障matrixは作らない。',
                                                         '原3のcontract_coverage/failure_attribution/integrationへ新固定候補の表示証拠と影響評価を結び、既存artifact_binding/206load-restart/原139planとhumanREAL延期を保持する。未影響の保証は実差分/条件に基づき既存impact '
                                                         'routeで再利用し、About再起動後の既存延期を増減しない。'],
                                               'expected': '正式descriptorは0.2.0のまま、初回Aboutに日本語説明と実ロードJAR由来のfullsource '
                                                           'SHAが明示される。identity不明/不正はunknown/reasonであり捏造しない。情報表示はcapture '
                                                           'helper準備・外部git/network/SDKやCLI/capture設定の変更を伴わない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/387',
                                                              'https://github.com/shinma06/android-replay-buffer/issues/280',
                                                              'docs/verification/changes/issue-3.json',
                                                              'docs/release-delivery.md',
                                                              'docs/verification/responsibility.md'],
                                               'gpt': {'status': 'pending',
                                                       'reason': '修正source/回帰を準備中。新固定正式ZIP/実ロードの初回Aboutと元F8-01の3保証は未観察。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。原human/REALと再起動後Aboutの延期を保持し、新しい本人必須gateを作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'recheck': 'About表示/同梱identity/descriptorとaction接続の影響を別04が固定HEAD/baseで評価する。修正版の元3行表示保証は実artifactに結び独立評価し、unit/build成功や旧e22実表示を新PASSへ転記しない。',
                                               'next_action': '通常required checks/currentHEAD '
                                                              'feedback/04独立source '
                                                              'review後、Rootが一度の標準正式候補と限定初回Aboutを既存担当へ割り当てる。残る独立試験は既存QA/release280/271へ双方向移管し、自己merge/正式公開しない。'}]}


RABBIT_CASES = {'docs/verification/changes/issue-391.json': [{'artifact': 'plugin',
                                               'gpt': {'status': 'pending',
                                                       'reason': '初回source準備のみ。新Mac SDK '
                                                                 'compile/build/実ロードとaffected受入は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存human/REAL延期を保持し、新しい本人承認gateを作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'id': 'RABBIT2-LOGCAT-TARGET',
                                               'change': '新exact buildをruntime '
                                                         'gateへ追加し、旧buildと381の対象端末・取消し・有限待機保証を維持する',
                                               'preconditions': '同じclean固定source/標準ZIP/全3JARと実ロード、AI-262.10968.63.2622.16730706、実Mac '
                                                                'JBRを同定する。共有host復旧後にRootが既存QA343/280の所有と調整し、旧381/387・7/294の観察/原FAIL/未達は変更しない。',
                                               'steps': ['LogcatToolWindowButtonTestの既存admission回帰で旧exactと新exactの受理、双方の未確認隣接build・別product・短縮buildの拒否を確認する。実SDKのLogcatTargetDeviceTestで既存のserial/online/nativeモデル、一覧到着、取消し/手動変更/対象変更/dispose/保存ログ/分割tabと元クリック起点の有限待機を確認する。',
                                                         '新IDEで正規対象Aと選択中Bを識別し、選択中の単一端末パネルを開く操作の実selector、native接続、Aのstream/描画を区別して確認する。初回/復元offline-to-online等の381保証は既存回帰と不足する実接続へ分担し、model一致だけで実描画PASSにしない。',
                                                         '閉じる・IDE側の手動開閉同期・Tab/Space・理由の可読性を確認し、tab/filterと他project、取得/Saveを維持する。旧Rabbit1 '
                                                         'exactの対応を除かず、旧成功/失敗を新buildの結果にしない。'],
                                               'expected': '新exactのGate拒否を解消し、型付きbridgeと381の元のserial/有限待機/取消し/非対応拒否/実stream保証を維持する。全262系の互換性、Windows静的ABIからのMac/実描画PASSを主張しない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/391',
                                                              'docs/verification/changes/issue-381.json',
                                                              'docs/verification/responsibility.md'],
                                               'recheck': '28 required member '
                                                          'descriptorのWindows静的一致は予備資料として保持し、新Mac '
                                                          'SDK/JBR/API compileと実接続の不足は新固定候補で別途確認する。',
                                               'next_action': 'BFの既存復旧・資源引継ぎ完了後、Rootが新Mac '
                                                              'SDK検証と固定source/ZIPの独立04レビュー・必要なnative担当を割当する。'},
                                              {'artifact': 'plugin',
                                               'gpt': {'status': 'pending',
                                                       'reason': '初回source準備のみ。新Mac SDK '
                                                                 'compile/build/実ロードとaffected受入は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存human/REAL延期を保持し、新しい本人承認gateを作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'id': 'RABBIT2-SETTINGS-SCREENSHOT',
                                               'change': '新IDE/SDK境界でSettingsとLogcat保存先連携を確認する',
                                               'preconditions': '新build/JBR/固定ZIPを同定し、旧app/profileを上書きしない専用fixtureと通常返却を確保する。旧Settings/スクリーンショットの原事実は別identityで保持する。',
                                               'steps': ['取得OFFの初期ロードと日本語ToolWindow/Settings、実modelを照合し、既存値/Apply/Cancel/reopenと有効状態を確認する。既存の設定schema/キーとCLI設定互換を変更しない。',
                                                         '新SDKで既存のLogcatScreenshotSettings/ReplaySettings/Environment系回帰を行い、必要な実連携でLogcatの保存先を読み、正しい所有先へ1枚のPNGが保存されることを確認する。manual保存先は未影響保証の適格な既存証拠を評価し、不必要な再実行を増やさない。',
                                                         '内部resolver等の未知/LinkageErrorで別folderへfallbackしない理由を保ち、元Settings/Run/plugins/完成出力と通常返却を照合する。'],
                                               'expected': '新IDEで適用・取消し・永続化と実保存先/画像の自製品glueを維持する。build成功だけでUIや保存効果をPASSにせず、他人の設定/成果物を破壊しない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/391',
                                                              'docs/plugin-development.md',
                                                              'docs/verification/changes/issue-341.json',
                                                              'docs/verification/responsibility.md'],
                                               'recheck': '新Mac '
                                                          'API/JBRによる影響と必要な実操作だけを評価し、旧F6等の観察を新版の実行へ付け替えない。',
                                               'next_action': 'Rootが共有host復旧・資源/leaseを確定後に、既存の検証責任と調整してaffected '
                                                              'Settings/保存先事実を割当する。'},
                                              {'artifact': 'plugin',
                                               'gpt': {'status': 'pending',
                                                       'reason': '初回source準備のみ。新Mac SDK '
                                                                 'compile/build/実ロードとaffected受入は未実施。'},
                                               'human': {'status': 'pending',
                                                         'reason': '未観察。既存human/REAL延期を保持し、新しい本人承認gateを作らない。'},
                                               'fix_issue': None,
                                               'fix_pr': None,
                                               'id': 'RABBIT2-CORE-CLI-RETURN',
                                               'change': '新Mac runtimeで共通capture '
                                                         'core/CLI/保存と通常返却のaffected境界を確認する',
                                               'preconditions': '新Mac '
                                                                'build/JBR、同梱CLIのjava/classpath、source/ZIP/全JARとendpoint/projectを同定する。既存app/profile/完成出力と原失敗は保持する。',
                                               'steps': ['新SDKの標準compile/check/buildPlugin/verifyPluginStructureと必要なPlugin '
                                                         'Verifier、既存CLI/設定/取得保存/終了のaffected回帰を実行する。同じ固定候補のbundled '
                                                         'CLIから正規project/status/版・認証・有限待機を確認する。',
                                                         '限定fixtureで必要なON/OFFとcapture-to-saveの内容/時刻/窓/失敗pin/atomicityと実保存導線を確認する。server/clock/JCodecのbyte不変などは保証別に適格な既存回帰/FILE/consumer証拠を評価し、新SDK更新だけを理由に全履歴や性能を反復しない。',
                                                         'current own '
                                                         'process/SDK/endpointと元profile/settings/Run/plugins/完成出力、停止/復元、lease '
                                                         'release ACKと別status '
                                                         'FREEをそれぞれ確認する。未達/UNKNOWNは保全し、未確認の新host効果を成功にしない。'],
                                               'expected': '同じ製品coreとCLI互換を維持し、変わったruntimeの実接続と保証上の必要な事実だけを新identityで確認する。追加性能計測・未検証の再利用・見かけの即時cleanup成功を作らない。',
                                               'provenance': ['https://github.com/shinma06/android-replay-buffer/issues/391',
                                                              'docs/cli.md',
                                                              'docs/verification/responsibility.md',
                                                              'docs/operations.md'],
                                               'recheck': 'SDK/host変更の影響が不明な保証はaffected再確認とし、原FAILを保持する。新ZIPだけで未影響の成功を無条件失効させず、正式なimpact/reuse条件と独立評価を使う。',
                                               'next_action': '通常checksと独立固定レビューを満たし、Rootが資源/既存復旧/共有GUIを調整して残試験を割当する。実装準備だけでclose/merge/main反映しない。'}]}
