use regex::Regex;
use std::{borrow::Cow, sync::LazyLock};

// Keep this projection in step with VCPChat's Groupmodules/toolResultContext.js.
const KEEP_BLOCK_MAX_CHARS: usize = 1500;

static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(^|\n)<<<\[ROLE_DIVIDE_USER\]>>>\n(?s:(.*?))\n<<<\[END_ROLE_DIVIDE_USER\]>>>(\n|$)",
    )
    .expect("valid desktop tool-result block pattern")
});
static TOOL_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"- 工具名称: ([^\n]+)\n(?:- 回复成员: [^\n]+\n)?- 执行状态: ([^\n]+)")
        .expect("valid desktop tool-status pattern")
});
static MEMBER_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[VCP调用结果信息汇总:\n- 工具名称: AgentAssistant\n(- 回复成员: [^\n]+\n)")
        .expect("valid AgentAssistant display-name pattern")
});

fn js_trim(text: &str) -> &str {
    text.trim_matches(|ch: char| ch == '\u{feff}' || (ch.is_whitespace() && ch != '\u{0085}'))
}

pub(super) fn condense(text: &str) -> Cow<'_, str> {
    if !text.contains("<<<[ROLE_DIVIDE_USER]>>>") {
        return Cow::Borrowed(text);
    }

    let mut scan = 0;
    let mut copied = 0;
    let mut result = None;
    while let Some(block) = BLOCK.captures_at(text, scan) {
        let whole = block.get(0).expect("complete match");
        // Rust regex consumes the ending newline. Leave it available so that
        // the next block can use it as its leading newline, as on desktop.
        scan = whole.end() - block[3].len();
        let body = &block[2];
        // Desktop counts UTF-16 code units and excludes display-only speaker
        // lines in AgentAssistant receipts from the threshold and summary.
        let display_chars: usize = MEMBER_LINE
            .captures_iter(body)
            .map(|line| line[1].encode_utf16().count())
            .sum();
        let count = body.encode_utf16().count() - display_chars;
        if count <= KEEP_BLOCK_MAX_CHARS {
            continue;
        }

        let mut tools = TOOL_LINE
            .captures_iter(body)
            .map(|tool| {
                let status = tool[2].replace(['✅', '❌'], "");
                format!("{} {}", js_trim(&tool[1]), js_trim(&status))
            })
            .peekable();
        let output = result.get_or_insert_with(String::new);
        output.push_str(&text[copied..whole.start()]);
        output.push_str(&block[1]);
        output.push('[');
        if tools.peek().is_none() {
            output.push_str("工具调用");
        }
        for (index, tool) in tools.enumerate() {
            if index != 0 {
                output.push('；');
            }
            output.push_str(&tool);
        }
        output.push_str(&format!(
            "；返回原文 {count} 字已从上下文省略，需要时重新读取]"
        ));
        copied = scan;
    }

    match result {
        Some(mut output) => {
            output.push_str(&text[copied..]);
            Cow::Owned(output)
        }
        None => Cow::Borrowed(text),
    }
}
