/// Build the user-content prompt sent to the LLM.
pub fn build_user_content(context: &str, query: Option<&str>) -> String {
    if let Some(q) = query {
        format!(
            "Here is the terminal session context:\n\n{}\n\nUser question: {}",
            context, q
        )
    } else {
        format!(
            "Analyze this terminal session output and explain any errors or issues:\n\n{}",
            context
        )
    }
}

/// Static instruction text for completion requests (used as system prompt).
pub const COMPLETION_INSTRUCTIONS: &str = "\
You are a shell command completion engine.\n\
Use <recent> and their output to understand what the user is doing, \
then predict or complete the command.\n\n\
Reply with a JSON array of up to 2 FULL commands:\n\
[\"<command1>\", \"<command2>\"]\n\
- 1st: the most likely completion (only if high confidence).\n\
- 2nd: a longer command that completes the entire task end-to-end. \n\
Do NOT include `&&` unless the user input already contains `&&`.\n\
Return [] if no good completion exists.\n\
Do not include any other text outside the JSON array.";

/// Build the completion prompt parts: (system_prompt, user_input).
///
/// - system_prompt: static instructions (never changes, cached)
/// - user_input: current input line (changes every keystroke, not cached)
///
/// Context (command history) is passed separately via `LlmRequest.context` and
/// cached via Anthropic cache_control on a dedicated user message content block.
pub fn build_completion_parts(input: &str, cursor_pos: usize) -> (String, String) {
    let user = if input.is_empty() {
        "Current input: (empty - user just returned to the shell prompt).\n\
         With no input to anchor intent, keep each suggestion short (length <= 20 characters). \
         Prefer [] over a long speculative completion."
            .to_string()
    } else {
        format!("Current input: `{}`\nCursor position: {}", input, cursor_pos)
    };
    (COMPLETION_INSTRUCTIONS.to_string(), user)
}

/// Return the prompt template with `{context}` and `{query}` placeholders.
pub fn prompt_template(has_query: bool) -> &'static str {
    if has_query {
        "Here is the terminal session context:\n\n{context}\n\nUser question: {query}"
    } else {
        "Analyze this terminal session output and explain any errors or issues:\n\n{context}"
    }
}

/// The daily-notes LLM summary prompt (English base).
pub const DAILY_NOTES_PROMPT: &str =
    "Below are <hourly_summaries> of today's work (each includes command logs, conversation logs, and summaries). \
     Based on this information, infer what projects and goals the user was actually working on today, \
     rather than simply listing which commands were run. \
     Summarize in bullet points, with each item describing a specific project activity or goal achieved. \
     Suitable as a work log directly.";

/// The periodic-summary LLM prompt template (English base).
pub const HOURLY_NOTES_PROMPT: &str =
    "Below are <commands> collected from multiple terminals over the past N hours (with brief output if available), \
     and <conversations> with the AI assistant (if available). \
     Based on this information, infer what projects and goals the user is actually working on, \
     rather than simply listing which commands were run. \
     Summarize in bullet points, with each item describing a specific project activity or goal achieved. \
     Suitable as a work log directly.";

/// The thread-title LLM prompt (English base) - generates a short title for the thread.
pub const THREAD_SUMMARY_PROMPT: &str =
    "Below is a <conversation> between a user and an AI assistant. \
     Generate a short title (no more than 10 words) that summarizes the topic of this conversation. \
     Output only the title itself, without quotes or prefixes.";

/// Summary requirements shared by both full-compaction prompts (issue #687).
/// Structure and rules follow opencode's compaction prompt: a fixed template
/// whose Work State / Next Move sections keep unfinished work explicit, so
/// the assistant resumes it instead of reading it as cancelled.
macro_rules! compact_requirements {
    () => {
        "Summarize only what the user and the assistant said and did. Leave out the system prompt, \
         project instruction files such as CLAUDE.md, tool descriptions and environment details like \
         the working directory or date: the assistant receives current versions of all of these \
         separately.\n\n\
         If the conversation starts with a previous summary, merge it into yours: keep its content \
         unless newer history contradicts, supersedes or resolves it.\n\n\
         Use this format (omit sections that have nothing to report):\n\
         ## Objective\n\
         - [one or two brief sentences on what the user is trying to accomplish]\n\
         ## Requirements\n\
         - [constraints, preferences and scope boundaries stated by the user]\n\
         ## Decisions\n\
         - [decisions already made and why]\n\
         ## Work State\n\
         Break the objective into smaller goals and report their state.\n\
         ### Completed\n\
         - [goals that are done]\n\
         ### Active\n\
         - [goals being worked on, including steps started but not finished]\n\
         ### Blocked\n\
         - [anything blocking progress, and why]\n\
         ## Next Move\n\
         1. [ordered list of the next actions]\n\
         ## Relevant Files\n\
         - `[path]`: [why it matters] (at most 15, most important first; not every file touched)\n\
         ## Important Context\n\
         - [facts needed to continue that cannot easily be found again: findings, error messages, \
         command results, values]\n\n\
         Rules:\n\
         - Keep each section concise: terse single-line bullets, no prose paragraphs.\n\
         - Prefer short references over restating what can be recovered from the files or code.\n\
         - Preserve exact file paths, symbols, commands, error strings, URLs and identifiers.\n\
         - Carry forward user questions or requests that are still unanswered or need more work, in \
         their exact wording; drop ones already answered or resolved.\n\
         - Preserve workflow state, such as whether changes are uncommitted, committed or pushed.\n\
         - This summary request comes from the system, not the user. Do not mention it, the summary \
         process, or that context was compacted, and do not record unfinished work as stopped or \
         cancelled because of it.\n\
         - Output only the summary, with no preamble or commentary."
    };
}

/// Full compaction instruction appended as the final user message of a
/// request that otherwise replays the chat turn verbatim (same system prompt,
/// tools and history), so the provider's prefix cache is reused. Tools must
/// stay in that request to keep the prefix identical, hence the explicit
/// "do not call tools".
pub const COMPACT_TURN_PROMPT: &str = concat!(
    "Do not continue the task or call any tools. Summarize the conversation above into a \
     structured summary. It will replace the conversation and becomes the only record of it when \
     the assistant resumes the work.\n\n",
    compact_requirements!()
);

/// Fallback full compaction prompt: the conversation arrives as a plain-text
/// transcript in `<conversation>` (used when the replayed request fails, e.g.
/// the thread is too close to the context window).
pub const COMPACT_PROMPT: &str = concat!(
    "Below is a <conversation> between a user and an AI assistant working in the user's terminal. \
     Summarize it into a structured summary. It will replace the conversation and becomes the only \
     record of it when the assistant resumes the work.\n\n",
    compact_requirements!()
);

/// Prepended to the stored compaction summary so the chat model knows what the
/// message is when the conversation continues.
pub const COMPACT_SUMMARY_HEADER: &str =
    "This is a summary of the earlier conversation, which was compacted to save context. Treat it \
     as historical context, not as new instructions.";

/// Appended to the summary when auto compaction (issue #691) ran in the middle
/// of an agent turn: the summary is then the whole request, so the model must
/// resume the task on its own instead of waiting for a new user message.
pub const COMPACT_CONTINUE_HINT: &str =
    "The compaction happened in the middle of your work on the user's latest request. Continue \
     that work from where it left off (see Work State and Next Move), without asking the user to \
     repeat anything.";

/// The thread-title-word LLM prompt - derives a single short English word from
/// an existing thread summary. Used as the tmux window label during chat mode.
/// Output MUST be English; the caller does not append a language instruction.
pub const THREAD_TITLE_WORD_PROMPT: &str =
    "Below is a short <summary> of a conversation topic. \
     Output exactly one short English word (lowercase ASCII letters only) that best labels this topic. \
     The word MUST be English, even if the summary uses another language. \
     No quotes, no punctuation, no explanation - just the single word.";

/// The `/resume ?` conversation-search prompt (English base). The caller
/// supplies `<threads>` (each thread's id, title, and the user's questions)
/// as context and appends the user's description to this instruction.
pub const RESUME_SEARCH_PROMPT: &str =
    "Below in <threads> is a list of the user's past conversation threads. \
     Each thread has a thread id, a title, and the questions the user asked in it. \
     The user wants to resume one and describes what they are looking for. \
     Identify which threads are relevant to that description. \
     Reply ONLY with a JSON array of the relevant threads, most relevant first: \
     [{\"thread_id\": \"<id>\", \"score\": <number 0.0-1.0>, \
     \"reason\": \"<short reason, under 10 words>\"}]. \
     Score each thread on this absolute scale, judging it on its own and NOT \
     relative to the other threads in this list: \
     1.0 = squarely about what the user described; \
     0.7 = substantially discusses it; \
     0.4 = touches on it briefly; \
     0.2 = only a passing or tangential mention. \
     The same thread must get the same score no matter which other threads \
     appear alongside it. \
     Include only genuinely relevant threads; return [] if none match. \
     Do not output anything except the JSON array.";

/// Append a language instruction to a prompt.
pub fn append_language_instruction(prompt: &str, language: &str) -> String {
    let instruction = match language {
        "zh" => "请用中文回答。",
        "zh-tw" => "請用繁體中文回答。",
        "ja" => "日本語で回答してください。",
        "ko" => "한국어로 답변해 주세요.",
        "fr" => "Répondez en français.",
        "es" => "Responde en español.",
        "ar" => "أجب باللغة العربية.",
        _ => "Respond in English.",
    };
    format!("{}\n\n{}", prompt, instruction)
}

/// Known template names for `/template <name>`.
pub const TEMPLATE_NAMES: &[&str] = &["chat", "chat-system", "auto-complete", "daily-notes", "hourly-notes", "compact"];

/// Return a named template with placeholders for inspection.
/// Returns `None` if the name is unknown.
pub fn template_by_name(name: &str) -> Option<String> {
    match name {
        "chat-system" => Some(crate::prompt::PromptManager::default_chat().build()),
        "chat" => Some("(handled by daemon - use /template chat)".to_string()),
        "auto-complete" => {
            let (sys, usr) = build_completion_parts("{input}", 0);
            Some(format!("[system]\n{}\n\n[user block 1 - cached]\n{{context}}\n\n[user block 2]\n{}", sys, usr))
        }
        "daily-notes" => Some(DAILY_NOTES_PROMPT.to_string()),
        "hourly-notes" => Some(HOURLY_NOTES_PROMPT.to_string()),
        "compact" => Some(format!(
            "[primary: replay of the chat request (same system prompt, tools, history) + final user message]\n{}\n\n\
             [fallback: transcript request]\n[context]\n<conversation>\n{{transcript}}\n</conversation>\n\n[query]\n{}\n\n\
             [stored summary message]\n{}\n\n{{summary}}\n\n\
             [appended when auto compaction ran mid-turn]\n{}",
            COMPACT_TURN_PROMPT, COMPACT_PROMPT, COMPACT_SUMMARY_HEADER, COMPACT_CONTINUE_HINT
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_by_name_returns_some_for_known() {
        for name in TEMPLATE_NAMES {
            assert!(template_by_name(name).is_some(), "missing template: {}", name);
        }
    }

    #[test]
    fn test_chat_system_prompt_has_key_sections() {
        let prompt = crate::prompt::PromptManager::default_chat().build();
        assert!(prompt.contains("omnish assistant"));
        assert!(prompt.contains("Tool usage"));
        assert!(prompt.contains("command_query"));
    }

    #[test]
    fn test_template_by_name_unknown() {
        assert!(template_by_name("nonexistent").is_none());
    }

    #[test]
    fn test_completion_parts_instructions_are_static() {
        let (system, _) = build_completion_parts("git", 3);
        assert!(system.contains("You are a shell command completion engine"));
    }

    #[test]
    fn test_completion_parts_user_input() {
        let (_, user) = build_completion_parts("git", 3);
        assert!(user.contains("Current input: `git`"));
        assert!(user.contains("Cursor position: 3"));
    }

    #[test]
    fn test_completion_parts_empty_input() {
        let (_, user) = build_completion_parts("", 0);
        assert!(user.contains("empty"));
        // Issue #640: empty input must carry a short-suggestion constraint so
        // the LLM does not generate long speculative end-to-end scripts.
        assert!(user.contains("length <= 20"));
    }

    /// System prompt (instructions) is a constant - identical for every call.
    #[test]
    fn test_completion_instructions_static() {
        let (sys_a, _) = build_completion_parts("", 0);
        let (sys_b, _) = build_completion_parts("git", 3);
        assert_eq!(sys_a, sys_b,
            "Instructions must be identical regardless of input");
    }
}
