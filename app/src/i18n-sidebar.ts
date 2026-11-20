export const SIDEBAR_EN: Record<string, string> = {
  编辑主机: "Edit host",
  新建主机: "New host",
  "还没有主机。点右上角 ＋ 新建，或从下面导入 ~/.ssh/config。":
    "No hosts yet. Click ＋ in the top-right to add one, or import ~/.ssh/config below.",
  "没找到匹配的主机。": "No matching hosts.",
  未分组: "Ungrouped",
  "还没有主机。先去「主机」页加一台。": "No hosts yet. Add one on the Hosts page first.",
  已连接: "Connected",
  "没有填私钥路径": "No private key path configured",
  更多: "More",
  连接: "Connect",
  编辑: "Edit",
  复制一份: "Duplicate",
  删除: "Delete",
  "{name} 副本": "{name} (copy)",
  "删除主机「{name}」？\n\n{target}\n\n这个操作不可撤销。":
    "Delete host “{name}”?\n\n{target}\n\nThis action cannot be undone.",
  "这台主机引用的密钥已经不在了（{keyId}），请重新选一把":
    "The key referenced by this host no longer exists ({keyId}). Choose another key.",
  代敲库: "TermMate vault",
  "这个私钥文件权限太松，SSH 会拒绝用它":
    "This private key file's permissions are too open; SSH will refuse to use it",
  "改成 600": "Set mode to 600",
  "这把密钥有口令，连接时需要解锁（钥匙串还没做）":
    "This key is passphrase-protected and must be unlocked when connecting (Keychain support is not implemented yet)",
  "另外 {count} 台主机也在用这把密钥：{names}":
    "{count} other host is also using this key: {names}",
  "另外共有 {count} 台主机也在用这把密钥：{names}":
    "{count} other hosts are also using this key: {names}",
  "、": ", ",
  选择私钥文件: "Choose private key file",
  "打不开文件选择器：{error}": "Could not open the file picker: {error}",
  "已导入「{name}」": "Imported “{name}”",
  "权限已改成 600": "Permissions set to 600",
  "先把主机保存一下（要拿到它的 id 才能存密码）":
    "Save the host first (its ID is required to save the password)",
  "密码是空的": "Password is empty",
  已存进钥匙串: "Saved to Keychain",
  密码已存进钥匙串: "Password saved to Keychain",
  "清掉这台主机存在钥匙串里的密码？": "Remove this host's password from Keychain?",
  "还没存密码，在这里填上": "No password saved yet; enter one here",
  已清除: "Cleared",
  "先保存这台主机，再测试连接": "Save this host before testing the connection",
  "连接中…": "Connecting…",
  "认证通过 · {seconds}s": "Authentication succeeded · {seconds}s",
  "正在看 agent…": "Checking agent…",
  "{kind} 里有 {count} 把密钥，连接时逐把试":
    "{kind} has {count} key loaded; each key will be tried when connecting",
  "{kind} 里共有 {count} 把密钥，连接时逐把试":
    "{kind} has {count} keys loaded; each key will be tried when connecting",
  "{kind} 在跑但没加载密钥，先加载再连接":
    "{kind} is running but has no keys loaded. Load a key before connecting.",
  "已存进钥匙串（不回显；要换就输新的再存）":
    "Saved to Keychain (never displayed; enter a new password and save to replace it)",
  "还没存密码，保存前在这里填上": "No password saved yet; enter one here before saving",
  "名称、地址、用户名都要填": "Name, address, and username are required",
  已保存: "Saved",
  "删除主机「{name}」？这个操作不可撤销。":
    "Delete host “{name}”? This action cannot be undone.",
  "~/.ssh/config 里没有可导入的主机（通配符条目和已经导入过的会自动跳过）。":
    "No hosts can be imported from ~/.ssh/config (wildcard entries and existing hosts are skipped automatically).",
  "没有可导入的": "Nothing to import",
  "config 里有 IdentityFile": "IdentityFile is set in config",
  "导入选中的 {count} 台": "Import {count} selected host",
  "导入选中的 {count} 台主机": "Import {count} selected hosts",
  "已导入 {count} 台主机": "Imported {count} host",
  "已成功导入 {count} 台主机": "Imported {count} hosts",
  选择已有分组: "Choose an existing group",
  "选择已有分组，或输入新分组": "Choose a group or type a new one",
  不分组: "Ungrouped",
  "新建分组「{name}」": "Create group “{name}”",
  "继续输入即可创建新分组": "Keep typing to create a new group",
};

export const SIDEBAR_JA: Record<string, string> = {
  编辑主机: "ホストを編集",
  新建主机: "新規ホスト",
  "还没有主机。点右上角 ＋ 新建，或从下面导入 ~/.ssh/config。":
    "ホストがまだありません。右上の＋から追加するか、下の ~/.ssh/config から読み込んでください。",
  "没找到匹配的主机。": "一致するホストはありません。",
  未分组: "グループなし",
  "还没有主机。先去「主机」页加一台。":
    "ホストがまだありません。まず「ホスト」ページで追加してください。",
  已连接: "接続済み",
  "没有填私钥路径": "秘密鍵のパスが未設定です",
  更多: "その他",
  连接: "接続",
  编辑: "編集",
  复制一份: "複製",
  删除: "削除",
  "{name} 副本": "{name} のコピー",
  "删除主机「{name}」？\n\n{target}\n\n这个操作不可撤销。":
    "ホスト「{name}」を削除しますか？\n\n{target}\n\nこの操作は取り消せません。",
  "这台主机引用的密钥已经不在了（{keyId}），请重新选一把":
    "このホストが参照している鍵（{keyId}）は削除されています。別の鍵を選択してください",
  代敲库: "タームメイトキーボールト",
  "这个私钥文件权限太松，SSH 会拒绝用它":
    "秘密鍵ファイルの権限が緩すぎるため、SSH がこの鍵を拒否します",
  "改成 600": "権限を 600 に変更",
  "这把密钥有口令，连接时需要解锁（钥匙串还没做）":
    "この鍵にはパスフレーズがあります。接続時にロック解除が必要です（キーチェーン連携は未対応）",
  "另外 {count} 台主机也在用这把密钥：{names}":
    "この鍵は他の {count} 台のホストでも使用されています：{names}",
  "另外共有 {count} 台主机也在用这把密钥：{names}":
    "この鍵は他の {count} 台のホストでも使用されています：{names}",
  "、": "、",
  选择私钥文件: "秘密鍵ファイルを選択",
  "打不开文件选择器：{error}": "ファイル選択ダイアログを開けませんでした：{error}",
  "已导入「{name}」": "「{name}」を読み込みました",
  "权限已改成 600": "権限を 600 に変更しました",
  "先把主机保存一下（要拿到它的 id 才能存密码）":
    "先にホストを保存してください（パスワードの保存には ID が必要です）",
  "密码是空的": "パスワードが空です",
  已存进钥匙串: "キーチェーンに保存済み",
  密码已存进钥匙串: "パスワードをキーチェーンに保存しました",
  "清掉这台主机存在钥匙串里的密码？": "このホストのパスワードをキーチェーンから削除しますか？",
  "还没存密码，在这里填上": "パスワードは未保存です。ここに入力してください",
  已清除: "消去しました",
  "先保存这台主机，再测试连接": "接続をテストする前にこのホストを保存してください",
  "连接中…": "接続中…",
  "认证通过 · {seconds}s": "認証に成功しました · {seconds}s",
  "正在看 agent…": "Agent を確認中…",
  "{kind} 里有 {count} 把密钥，连接时逐把试":
    "{kind} に {count} 個の鍵が登録されています。接続時に順番に試します",
  "{kind} 里共有 {count} 把密钥，连接时逐把试":
    "{kind} に {count} 個の鍵が登録されています。接続時に順番に試します",
  "{kind} 在跑但没加载密钥，先加载再连接":
    "{kind} は実行中ですが、鍵が登録されていません。先に鍵を登録してください",
  "已存进钥匙串（不回显；要换就输新的再存）":
    "キーチェーンに保存済み（表示はされません。変更するには新しいパスワードを入力して保存してください）",
  "还没存密码，保存前在这里填上": "パスワードは未保存です。保存前にここに入力してください",
  "名称、地址、用户名都要填": "名前、アドレス、ユーザー名は必須です",
  已保存: "保存しました",
  "删除主机「{name}」？这个操作不可撤销。":
    "ホスト「{name}」を削除しますか？この操作は取り消せません。",
  "~/.ssh/config 里没有可导入的主机（通配符条目和已经导入过的会自动跳过）。":
    "~/.ssh/config に読み込めるホストがありません（ワイルドカード項目と読み込み済みのホストは自動的に除外されます）。",
  "没有可导入的": "読み込める項目がありません",
  "config 里有 IdentityFile": "config に IdentityFile が設定されています",
  "导入选中的 {count} 台": "選択した {count} 台を読み込む",
  "导入选中的 {count} 台主机": "選択した {count} 台を読み込む",
  "已导入 {count} 台主机": "{count} 台のホストを読み込みました",
  "已成功导入 {count} 台主机": "{count} 台のホストを読み込みました",
  选择已有分组: "既存のグループを選択",
  "选择已有分组，或输入新分组": "既存のグループを選択、または新しい名前を入力",
  不分组: "グループなし",
  "新建分组「{name}」": "グループ「{name}」を作成",
  "继续输入即可创建新分组": "続けて入力すると新しいグループを作成できます",
};
