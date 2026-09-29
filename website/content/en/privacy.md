# Privacy policy

Effective 29 September 2026. This policy covers the Mework desktop app in every edition: the installer, the portable archive, and the MSIX package distributed through the Microsoft Store.

## In short

Mework collects nothing. There is no Mework account, no telemetry, no analytics and no crash reporting, and the developer runs no server that the app talks to. What you do in Mework stays on your computer, except the requests you direct it to make to services you choose.

## What stays on your computer

Settings, workspaces, conversations, attachments, memories and logs are stored in `%APPDATA%\com.mework.app` and `%LOCALAPPDATA%\com.mework.app`. API keys and sign-in tokens are kept in the operating system's credential store (the Windows Credential Manager), never in those files. Uninstalling the app does not delete this data; remove the two folders to delete it.

## What leaves your computer, and to whom

Only to services you configure, and only when you use them:

- **Model providers** you add (for example OpenAI, Anthropic, Google, Azure OpenAI, Amazon Bedrock, Google Vertex, xAI, DeepSeek or any OpenAI-compatible endpoint) receive each request's conversation content, the files and tool results it includes, and your API key or sign-in. Their own privacy policies govern that data.
- **OpenAI Codex** signs in with your ChatGPT account at OpenAI. The **Claude Agent** provider runs the Claude Code executable that ships with Mework, which signs in with your Anthropic account; Mework turns its telemetry and error reporting off.
- **Web search and fetch**: the search service you configure receives your queries; the sites the agent fetches, and the pages you open in the built-in browser, receive ordinary web requests.
- **MCP servers, hooks and SSH machines** you add receive what you configure them to receive.
- **Update check**: the installer and portable editions ask GitHub (`api.github.com`) for the latest release when you open *Settings → Updates*; GitHub sees your IP address and the app version. The Microsoft Store edition never checks; the Store updates it.
- **Local helper model**: when you choose to install it, its files are downloaded from Hugging Face.

## Children

Mework is a tool for developers and is not directed at children under 13.

## Changes

Changes to this policy are published on this page with a new effective date.

## Contact

Questions about this policy: open an issue at <https://github.com/catblob-hash/Mework/issues>.
