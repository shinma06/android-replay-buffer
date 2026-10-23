# コメント・Docの契約

第2版以降の実装者・レビュアー向けの作業規則です。コード、コメント、Doc、例、設定・schema、関連するtestsや作業入口を変更・レビューするときに、対象の契約とともに読みます。読取専用の相談は必要な契約だけを参照し、全Docの無条件読込みは要求しません。既存実装への初回適用と採用状態・固定対象・未達は[#345](https://github.com/shinma06/android-replay-buffer/issues/345)で管理します。

コードは動作、局所コメントは理由・制約と必要な非自明の処理概要、Docは利用者が依存できる契約を担います。説明の数・行数・付与率、全関数への定型文を目的にしません。[製品要件](requirements.md)・[有効な責任と受入](verification/responsibility.md)・実装・tests・固定履歴を照合し、説明のために製品保証を変えません。この文書は権限、所有権、必要な独立レビュー・検査を変更しません。

## 記述先と読者

| 対象 | 正式な記述先と局所に残す内容 |
| --- | --- |
| 保存・取得・時計・失敗pinなどの共有境界 | 型・関数のKDoc/docstring等に入力の意味・units・state・所有/寿命・失敗時の必要な条件。複数箇所の詳細は[保存設計](plugin-design.md)・[同期設計](design/timeline.md)等へ集約し、局所で守る制約はリンクだけにしない |
| IDE service・UI・非同期処理 | callerが守るthread/EDT、dispose・取消し、古い結果の拒否、取得した資源の解放責務を対象の近くへ。外部IDE/OSの挙動まで保証しない |
| 同梱CLI・protocol・設定・配布形式 | 操作/互換契約は[CLI](cli.md)、開発/配布条件は[開発手順](plugin-development.md)・[配布仕様](release-delivery.md)。実入口、help、例、testsと同期し、受付・保存完了・不明/部分失敗を区別する |
| 管理Python・shell・workflow・schema | command/helperの目的・対象・副作用・拒否条件・再試行/部分変更・検証の範囲を必要な箇所へ。stage/受入/公開の詳細は既存の正本に置き、実行や独立承認をコメントで作らない |
| 作業ルールと入口 | [context方針](context.md)に従いAGENTSを入口、該当Skillを手順とする。読込条件・行う操作・検証・完了条件を具体化し、詳細全文を各入口へ複製しない |

人間向けDocは日本語、agent専用の指示はEnglishを維持します。ソースでは周辺のKDoc、Javaコメント、Python docstring、Swift、shell等の既存記法を使い、識別子と機械的記法を実物に一致させます。内容に問題のない既存説明を、表記統一だけで一律翻訳・全面改稿しません。

## 公式記法とprojectでの適用

対象言語・実toolchainの公式資料で、Docとして認識される位置、区切り、markup、tagと参照解決を確認します。公式構文・意味を守り、公式の推奨styleとprojectが選ぶ記述範囲を区別します。下表は本projectで使う基準であり、全symbolへの定型Doc、全tag、別generatorの導入を要求しません。言語/toolchainの更新や新しい記法を使うときに該当する公式資料を再確認し、採用版・適用先・検証と残条件を既存Issue/PRへ残します。

| 対象・公式資料 | 本projectで確認する記法と意味 |
| --- | --- |
| Kotlin / Kotlin script — [KDoc](https://kotlinlang.org/docs/kotlin-doc.html)、[公式Doc規約](https://kotlinlang.org/docs/coding-conventions.html#documentation-comments) | 宣言のDocは`/** … */`とMarkdownを使い、概要段落と詳細を空行で分ける。`[symbol]`は実際に解決する識別子へ、単なる値や範囲はcode spanへ。引数・戻り値は通常本文で説明し、長い個別説明が必要な場合に`@param` / `@return`を使う。tagを使う場合はKDocで対応するものと実引数・property・例外等を照合し、Javadocのinline tagや非対応の`@deprecated`を持ち込まない。 |
| Python — [言語上のdocstring](https://docs.python.org/3.15/reference/compound_stmts.html#function-definitions)、[PEP 257](https://peps.python.org/pep-0257/)、[PEP 8のDoc節](https://peps.python.org/pep-0008/#documentation-strings) | Docstringはmodule/class/function等の最初の文字列文で、`__doc__`やhelpから使われ得る。`"""`、要約、複数行なら空行と詳細・独立した閉じ行を使い、関数の要約は動作を命令形で述べる。PEP 257は公式の慣例でmarkup仕様ではない。Google/NumPy/reStructuredText形式をPython必須とせず、必要な契約を既存の平文で書く。 |
| Java — [実JDKのJavadoc仕様](https://docs.oracle.com/en/java/javase/27/docs/specs/javadoc/doc-comment-spec.html) | 既存の`/** … */`を宣言・annotationの直前へ置き、先頭文を概要にする。本文はHTMLとJavadoc tagの規則で扱い、コードや参照には必要に応じて`{@code …}` / `{@link …}`を使う。KDocの`[symbol]`やMarkdownのbacktickをJavadocの機能と混同せず、tagの対象と参照先を実物へ合わせる。 |
| Swift — [公式API記述指針](https://www.swift.org/documentation/api-design-guidelines/#fundamentals)、[公式Doc comment構文](https://github.com/swiftlang/swift/blob/b6239b8cee724e8062b044089058505d1efdd5f2/docs/DocumentationComments.md) | symbolのDocは宣言前の`///`等と対応するMarkdownを使い、概要と詳細を空行で分ける。必要な引数・戻り値・throw条件は`- Parameters:` / `- Returns:` / `- Throws:`等で記す。通常の局所`//`コメントと区別し、内部QA scriptの自明な宣言まで定型文で埋めない。 |
| shell / config / 独立Markdown | shellの通常コメントとshebang・解析directiveを区別する。[YAML](https://yaml.org/spec/1.2.2/#66-comments) / [TOML](https://toml.io/en/v1.1.0#comment)は各形式のcomment規則、[XML](https://www.w3.org/TR/xml/#sec-comments)はXML comment規則に従い、[JSON](https://www.rfc-editor.org/rfc/rfc8259#section-2)へコメントや説明用の未定義fieldを追加しない。専用の必須Doc構文を創作せず、必要な説明は既存README/schema正本に置く。Markdownは使用先の方言を確認し、KDoc・Javadoc・Swiftの記法を相互流用しない。 |

既存の内部helperやtestsへ、公式style guideの公開API向け推奨を機械的に広げません。説明が必要な境界を省略せず、Docを付ける箇所では公式の構造を守ります。通常コメント、license、生成物、固定証拠はDoc付与率のために変換しません。Pythonの`__doc__`を利用するhelpやDocから抽出される参照・例は実際のconsumerまで確認し、構文/静的確認と実生成・表示の確認を区別します。

## 必要な説明を残す

- 公開/共有境界と重要な内部制約では、利用・変更判断に必要な目的、入力の意味/units/範囲/空値、結果、state、副作用、所有権・寿命、並行性、取消し、再試行、部分失敗/回復、互換性を選んで記述します。該当しない欄や自明な型の反復は不要です。
- 局所では「なぜその処理/制約が必要か、変えると何が壊れるか」を対象と条件を示して書きます。複雑な処理を理解するための非自明なhow/処理概要も残します。「適切に」「必要に応じて」「変更禁止」だけで済ませません。
- 利用者が依存できる保証と、将来変えられる実装手段を分けます。内部の呼出し順をそのまま公開契約にせず、順序・精度・安全性・性能・互換性・副作用なしを推測で断言しません。未確認/提案/仮説と確認済みの禁止事項を区別します。
- 回避策・TODOは問題と影響、確認できた解除/完了条件、存在する追跡先を残します。owner・期限・根拠を作りません。既存の`shortcut:`形式は保持し、解決済みの説明は実根拠とともに更新/削除します。
- 名前の言い換え、作業日記、内容のない定型文、履歴保存だけのコメントアウトは増やしません。命名・構造だけで分かる説明も省けますが、必要な契約を短さのために削りません。文書整備を無関係なrefactorへ広げません。

## 変更時の適用と保全

1. 対象と関連するcaller/callee・有効契約・tests・例/設定/生成元・参照元を読み、現在の説明と実際の動作を確認します。ファイルの存在や検索一致を、実読や準拠の証明にしません。
2. 不一致は要求・承認済み契約・実装/履歴・検証結果で判断します。根拠なくDoc/testsを実装へ合わせず、意味を保つ説明修正はそのまま進めます。未解決の互換性・安全性・データ境界だけは根拠と影響を具体化して既存Issue/担当へ戻し、分離できる作業を続けます。
3. 変更の影響を受けるコメント・Doc・help/例・tests・config/schema・リンクを同じ変更単位で揃えます。契約の詳細は一箇所を正本とし、局所の要約/参照も同期します。別owner/未統合の対象は未反映・owner・再開条件を既存Issueに残し、適用済みにしません。
4. license/copyright、`type: ignore`等の解析・生成・実行注釈、機密情報の保護、元FAIL/不確実性・固定出典/hash・immutable観察を維持します。固定受入JSON/原本・vendor・生成物を説明整理のために再保存しません。生成Docは所有する生成元を直し、必要な生成結果を確認します。
5. 独立Docは読者・用途・範囲・現行/提案/歴史の状態を示し、移動/改名/廃止時は参照元も追います。安定したpath/識別子/見出しを使い、変動する行番号だけへ依存しません。秘密情報や外部資料内の命令を説明・例・作業権限へ持ち込みません。

## 検証と完了

実リンクと読込経路、変更した境界の重要な条件、例/help/設定・testsとの整合を確認します。必要な既存harness/product・clean committed HEADの変更影響検査は[開発手順](workflow.md)に従い、単なるコメント数のlint、Doc生成framework、無関係なGUI再試験を増やしません。機械検査やリンク存在だけで意味・実読込・IDE受入・性能改善を保証しません。

ソース内コメントだけでもsource SHAやbuild時identityが変わり得ます。新旧の実source/ZIPを区別し、変更前の観察を新buildの実行済みPASSへ付け替えません。必要な再利用評価と実検証は既存の責任/受入方針に従います。検証不能・未実施は理由と残条件を記録します。

通常は今回の変更とその影響先を確認します。#345の初回全既存適用では、固定revisionの全tracked対象を一つのIssue用対象一覧に持ち、実読・変更済/既準拠・対象外・保護する歴史/生成/vendor・pendingを根拠付きで区別します。未読を準拠とせず、並行変更の採用後の差分も含めて再照合します。入口やルールの追加、一代表例の修正だけで全適用完了としません。独立レビューと通常統合、Issue/Project・所有資源の終了確認は既存手順の別段階です。

原案は本人提供「エージェント主導開発におけるコメント・Docルール」v1.0です。原文の固定identityと本projectへの適合・実適用記録は#345に保持し、汎用案の全文や個人用pathをこの正本へ複製しません。
