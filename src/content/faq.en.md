# SailTerm Help & Q&A (English corpus)

Maintenance note (this section starts with a single-# title, so it is not rendered as content):
Start each Q&A with a level-2 heading (hash + space + the question); the text beneath it is the answer, up to the next level-2 heading.
To add an entry, append one in that format below; to remove one, delete its section. Ship updates with a release — no component code changes needed.
Chinese corpus lives in faq.zh.md. Answers render as plain text with line breaks preserved; Markdown syntax such as bold or links is not rendered for now.

## [1] What software is SailTerm?
SailTerm is the app you are using right now: a desktop SSH / SFTP / FTPS terminal and file-management client with a built-in AI assistant. It is NOT FileZilla, WinSCP, Xshell or FinalShell — when answering any question about "the client / this tool", rely on what this app actually does instead of assuming features of other software.

## [2] Where can I see transfer records? Are past transfers still queryable?
In the Transfers panel of the bottom dock. Every upload/download is persisted to the app database: file, direction, progress, final state (done/failed/canceled) and the error reason — they survive app restarts. Failed or canceled tasks can be retried with resume. Two caveats: the list has a capacity cap and the oldest finished records are trimmed automatically; the toolbar "Clear" button removes finished rows of the current session, after which they are gone for good.

## [3] Does the app log which files were deleted? Can I find a record after a mistaken delete?
No. SailTerm keeps no file-deletion event log — whether you delete via the file pane, `rm` in the terminal, or an AI-executed command, there is no "who deleted what when" record (the AI command audit stores commands and their disposition, not deleted filenames). Your only recourse is server-side: recycle bin, snapshot, backup, or your server administrator.

## [4] How do I view or change file permissions (chmod)?
Right-click the file in the remote pane of an SFTP session → "Permissions…" to open the permission editor: checkbox matrix, octal and symbolic views stay in sync, and special bits (setuid/setgid/sticky) live in a collapsed "Special bits" drawer. It edits one file at a time — there is no batch chmod; for bulk changes just ask the AI to run the command in an SSH session.

## [5] Why is there no "Permissions…" entry for FTP files?
FTP has no standard chmod command (SITE CHMOD is a non-portable server extension), so the entry is hidden on FTP/FTPS panes. Use an SFTP (SSH file) session to change permissions.

## [6] Why can't the AI run commands in an FTP session?
FTP is a pure file-transfer protocol with no shell channel — it is a protocol limitation, not the AI refusing. In FTP sessions the AI only gets file tools (list dir, read file, search, upload/download); Linux command capabilities exist only on SSH sessions (including the SSH connection behind an SFTP file pane).

## [7] When I double-click a file, why is some opened inside the app and some externally?
It is decided by content: plain-text files open in the built-in editor; binary files (images, videos, archives, executables…) are handed to the OS default program (Windows associated app, macOS default). This is by design, not a download.

## [8] Can the AI search the web? How do I control that?
Yes, behind one master switch: Settings → Security → "AI network access". While enabled the AI has web_search and fetch_url, and anything leaving the app passes the sanitizer first; when disabled both tools vanish from the AI entirely. Note that cloud models themselves must reach their provider — this switch governs the AI's extra self-initiated web lookups.

## [9] What can the AI assistant actually do?
On SSH sessions: run commands (writes require your approval), list directories, read/write remote files, search files, upload/download, read terminal output, read your attached files, write task plans, search the web and fetch pages, and ask you clarifying questions. On FTP sessions only the file tools remain (see previous entry). Risk grading, approval and sanitizing are enforced by the orchestrator and cannot be bypassed by the AI.

## [10] How do I quickly summon the AI inside the terminal?
Two entries: 1) at a clean prompt, type @ (question) or # (instruction) as the first character on an empty line to open an inline mini-ask (Esc returns to the terminal); 2) select text in the terminal and use the shortcut to invoke inline AI. The right-side AI panel supports multi-turn chat and drives the full tool chain. In alternate-screen apps (vim/less/top) @ and # go to the program as usual and never trigger the AI.

## [11] How does the quick command bar work?
The chip strip at the bottom of the terminal is the quick command bar — clicking a chip sends the command straight to the shell. The "＋" at its right end opens the management panel, where commands live in three categories: "Global" (visible to every connection), "Current group" (visible to connections in the group) and "This connection" (private to this connection's bar). The ＋ beside a category creates straight into it; commands in the Global/group pools can be pulled onto this connection's bar all at once with "Apply all" or one by one via the ✓ box at the row start. Click a command's name to edit it in place; the × at the row end (appears on hover) deletes it from the command library after confirmation. Commands with ｛｛variable｝｝ placeholders ask for values before running. To manage a group's command pool in bulk, right-click the group in the sidebar and pick "Group quick commands".

