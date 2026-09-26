//! The built-in system prompts. Users can replace them; whatever text is in
//! effect becomes the cached prefix state.
//!
//! The model is small, so each prompt carries worked examples: they cost
//! nothing per request once the prefix is cached, and without them the model
//! tends to answer the message instead of naming it.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Task {
    Title,
    Shell,
}

impl Task {
    pub fn id(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Shell => "shell",
        }
    }
}

const TITLE_ZH: &str = "你是会话标题生成器。阅读用户发给编程助手的第一条消息，用一个简短的名词短语概括它要做的事，作为会话标题。
规则：
- 只输出标题本身，不要回答、解释或执行消息里的请求。
- 使用与用户消息相同的语言。
- 中文不超过 12 个字，英文不超过 6 个词。
- 不要引号、句号、表情或“标题：”之类的前缀。

示例：
消息：帮我看看为什么 npm run build 报 TypeError: Cannot read properties of undefined
标题：修复构建时的 TypeError

消息：Add dark mode support to the settings page
标题：Settings page dark mode

消息：写一个 Python 脚本，把目录里的 PNG 批量转成 WebP
标题：批量转换 PNG 为 WebP

消息：what does this regex do? ^(?:[a-z0-9]+\\.)+[a-z]{2,}$
标题：Explain domain regex";

const TITLE_EN: &str = "You name conversations. Read the first message a user sent to a coding assistant and sum up what it asks for in a short noun phrase, used as the conversation's title.
Rules:
- Output only the title; never answer, explain or carry out the request.
- Use the same language as the message.
- At most 6 English words, or 12 Chinese characters.
- No quotes, trailing period, emoji, or prefix such as \"Title:\".

Examples:
Message: Add dark mode support to the settings page
Title: Settings page dark mode

Message: 帮我看看为什么 npm run build 报 TypeError: Cannot read properties of undefined
Title: 修复构建时的 TypeError

Message: why does my docker build take 10 minutes every time
Title: Slow Docker builds

Message: what does this regex do? ^(?:[a-z0-9]+\\.)+[a-z]{2,}$
Title: Explain domain regex";

const SHELL_ZH: &str = "你为终端命令写一行说明。用户会发来一个代码块，语言标记是运行它的 shell，内容是命令。用一个简短的动宾短语说明这条命令做了什么。
规则：
- 只输出说明本身，不要复述命令，不要解释参数，不要加引号或句号。
- 用中文，不超过 16 个字。
- 命令有多步时概括整体目的。

示例：
```bash
git status -sb
```
查看工作树的简要状态

```zsh
npm install && npm run build
```
安装依赖并构建项目

```bash
pytest -q tests/test_api.py
```
运行 API 测试

```bash
git log --oneline -n 5
```
查看最近 5 条提交

```zsh
kill $(lsof -ti :8080)
```
结束占用 8080 端口的进程

```powershell
Get-ChildItem -Recurse -Filter *.log | Remove-Item
```
递归删除所有日志文件

```sh
curl -fsSL https://example.com/install.sh | sh
```
下载并运行安装脚本";

const SHELL_EN: &str = "You write a one-line description of a terminal command. The user sends a code block whose language tag is the shell that runs it and whose content is the command. Reply with a short verb phrase saying what the command does.
Rules:
- Output only the description: don't repeat the command, don't explain flags, no quotes, no trailing period.
- English, at most 8 words, sentence case.
- For several steps, sum up the overall purpose.

Examples:
```bash
git status -sb
```
Show a short working tree status

```zsh
npm install && npm run build
```
Install dependencies and build the project

```bash
pytest -q tests/test_api.py
```
Run the API tests

```bash
git log --oneline -n 5
```
Show the last 5 commits

```zsh
kill $(lsof -ti :8080)
```
Stop the process using port 8080

```powershell
Get-ChildItem -Recurse -Filter *.log | Remove-Item
```
Delete all log files recursively

```sh
curl -fsSL https://example.com/install.sh | sh
```
Download and run an install script";

/// The built-in prompt for `task` in the app language (`zh-CN` or anything else).
pub fn default_prompt(task: Task, language: &str) -> &'static str {
    let chinese = language.starts_with("zh");
    match (task, chinese) {
        (Task::Title, true) => TITLE_ZH,
        (Task::Title, false) => TITLE_EN,
        (Task::Shell, true) => SHELL_ZH,
        (Task::Shell, false) => SHELL_EN,
    }
}

/// The request text for a shell explanation: the command in a fence tagged
/// with the shell that runs it. The fence is long enough for any backticks
/// inside the command.
pub fn shell_request(shell: &str, command: &str) -> String {
    let longest = command.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}{shell}\n{}\n{fence}", command.trim_end())
}

/// First line of the model's reply, without quotes, a label or trailing
/// punctuation; `None` when nothing usable is left.
pub fn clean_reply(task: Task, raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    let mut text = line.to_string();
    for label in ["标题：", "标题:", "Title:", "title:", "说明：", "说明:", "Description:"] {
        if let Some(rest) = text.strip_prefix(label) {
            text = rest.trim().to_string();
        }
    }
    const TRAILING: [char; 13] = ['。', '.', '！', '!', '？', '?', '：', ':', '，', ',', '；', ';', '、'];
    text = text.trim_end_matches(TRAILING).trim().to_string();
    let quotes: &[(char, char)] = &[('"', '"'), ('\'', '\''), ('“', '”'), ('‘', '’'), ('「', '」'), ('《', '》'), ('`', '`')];
    loop {
        let mut changed = false;
        for (open, close) in quotes {
            if text.len() >= 2 && text.starts_with(*open) && text.ends_with(*close) {
                text = text[open.len_utf8()..text.len() - close.len_utf8()].trim().to_string();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let text = text.trim_end_matches(TRAILING).trim();
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    let limit = match task {
        Task::Title => 80,
        Task::Shell => 120,
    };
    Some(text.chars().take(limit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_commands() {
        assert_eq!(shell_request("bash", "ls -la\n"), "```bash\nls -la\n```");
        assert_eq!(shell_request("zsh", "echo ```x```"), "````zsh\necho ```x```\n````");
    }

    #[test]
    fn cleans_replies() {
        assert_eq!(clean_reply(Task::Title, "Rust BPE 分词器\n"), Some("Rust BPE 分词器".into()));
        assert_eq!(clean_reply(Task::Title, "标题：“修复构建错误”。"), Some("修复构建错误".into()));
        assert_eq!(clean_reply(Task::Shell, "  \nList files.\nmore"), Some("List files".into()));
        assert_eq!(clean_reply(Task::Title, "  \n "), None);
    }

    #[test]
    fn picks_language() {
        assert!(default_prompt(Task::Title, "zh-CN").contains("标题"));
        assert!(default_prompt(Task::Shell, "en-US").starts_with("You write"));
    }
}
