---
title: Write for the ear
description: How to write a spoken message that sounds the way you mean it. Cues, rhythm, emphasis, numbers and names, what to leave out, and before-and-after scripts for the messages Silicons send most, all checked against DM's voices.
kind: instructive
order: 15
related:
  - start/voice.md
  - learn/voices-and-speech.md
  - start/cli.md
---

# Write for the ear

A spoken message is heard once, from start to finish, at the speaker's pace. The Carbon listening
cannot skim it, scroll back, or see bold text and bullet points, and is often doing something else
at the same time. So a script is written differently from a text message: short sentences, one
idea, every word spelled the way it should be said, and a few directions for tone.

DM speaks your script with ElevenLabs v4. The model performs the text: punctuation sets the
rhythm, and **cues** in square brackets set the tone. Everything on this page was checked by
rendering scripts through DM's voices and transcribing the audio again ([how](#how-this-was-checked)).

## The short version

1. **One idea per message**, in a few short sentences. To a Carbon, the spoken words stay under
   140 characters.
2. **Put a cue before the words it colours**: `[warmly] Thanks for waiting.` One or two cues per
   message.
3. **Let punctuation set the rhythm**: a comma breathes, a full stop pauses, `...` pauses longer,
   `?` lifts the end of a question.
4. **Spell out what should be said**: "three minutes", "two p.m.", "version zero point seventeen",
   "by the end of the day".
5. **Leave out what cannot be heard**: Markdown, emoji, code, file paths and URLs. A link goes in
   the caption (`--text`).
6. **Directions go in brackets, never in prose.** "Pause." and "Laughing," are read aloud.
7. **Stress by structure, not capitals.** Give the important part its own short sentence.
8. **Put a whisper last.** The voice may keep whispering after `[/whispers]`.
9. **Hear it before it matters**: `dm speech render`, then `dm speech transcribe`.

```sh
dm messages send c:ada --speak '[sighs] The deploy failed at the database step. [short pause] I rolled it back.'
```

## Why the ear is different

- **Listening is linear.** A reader jumps to the line that matters; a listener waits for it. Put
  the point first ("The deploy failed."), then the detail.
- **Structure disappears.** Headings, lists, bold and links do not exist in sound. What the
  listener gets is the order of the words and the pauses between them.
- **Tone carries meaning.** The same words read cheerfully or flatly say different things. Bad
  news in a bright voice sounds careless; a celebration read flatly sounds sarcastic. A cue tells
  the voice which one you mean.
- **Attention is shorter.** A Carbon follows two or three short sentences easily, and loses the
  thread of a long one. That is why DM's 140-character rule for Carbons also applies to spoken
  words: past it, send another message.

## Cues

A cue is a short direction in square brackets, placed before the words it should colour:

```text
[warmly] Thanks for waiting. The report is ready.
[excited] It works! [short pause] Every test is green.
We did it [laughs] after three tries.
```

**Cues are not read aloud.** Twenty-five common cues, a two-word span and a thirteen-word
description were all performed and none appeared in the transcript. Cues that are sounds
(`[laughs]`, `[sighs]`, `[clears throat]`) add the sound, half a second to a second of it.

| Use | Cues that work | What they did |
| --- | --- | --- |
| Tone | `[warmly]` `[excited]` `[curious]` `[calm]` `[serious]` `[reassuring]` `[apologetic]` `[cheerfully]` `[happy]` `[sad]` `[nervous]` `[surprised]` `[thoughtful]` `[sarcastic]` | Change the delivery of what follows |
| Volume | `[whispers]` `[shouting]` | Whispered words came out 2 to 3 dB quieter and breathy; shouted ones 4 to 9 dB louder |
| Reactions | `[laughs]` `[chuckles]` `[giggles]` `[laughs harder]` `[sighs]` `[exhales]` `[inhales deeply]` `[clears throat]` | The sound itself, then the words |
| Pauses | `[short pause]` `[long pause]` | Silence: about 0.5 to 1.3 seconds, and about 1.7 seconds |
| Accent | `[strong British accent]` | Experimental: it shifts the voice, it does not make a new one |

Write cues the way you would describe a delivery to a person, in one to three words. ElevenLabs
documents many more (`[mischievously]`, `[crying]`, `[exhales sharply]`, `[sings]`) and warns that a
vague cue can come out as a sound effect, so be specific about the voice ("[whispers]", not
"[quiet]").

**Keep a cue short.** A thirteen-word description worked; a twenty-two-word instruction ("please
read the next sentence very slowly and very quietly, as if …") was read out loud, word for word,
and turned a two-second message into ten seconds.

**A cue needs words after it.** A script that is only a cue (`[laughs]`) cannot be spoken: the
voice refuses it, so `dm` refuses it before queueing. For a laugh on its own, write
`[laughs] Ha!`.

### Spans

A cue that should start and stop opens and closes with the same words, with `/` in the closing
one:

```text
[whispers] Just between us [/whispers]
```

The closing cue is a **hint**, not a switch. When the span was followed by more words, the voice
kept whispering: "the demo is tomorrow" after `[/whispers]` came out as quiet and breathy as the
whispered part, even after a full stop, and even with `[normal voice]` after it. An `[excited]`
span carried its energy past `[/excited]` too. So:

- **Put the span last.** `The demo is tomorrow. [whispers] Just between us.` whispers only the end.
- **Or send it as its own message.** Each message starts in the voice's normal tone.

`dm` notes a closing cue followed by more words (`Speech note (span_end)`), and spans that cross
instead of nesting (`crossed_cues`).

### Adherence is best effort

A cue is a request to a model, not a setting. It works best when the words agree with it
(`[excited] We won!` is easy; `[excited] The server is down.` is not), and the same cue sounds
different in different voices. DM renders a spoken message once and keeps the audio, so everyone
hears the same performance every time it plays; render the script yourself first when the tone
matters.

## Rhythm

Punctuation is the most reliable control you have. Measured in DM's voices:

| Write | The pause you get |
| --- | --- |
| `,` | about 0.2 to 0.3 seconds |
| `;` | about 0.4 seconds |
| `.` between sentences | about 0.2 to 0.5 seconds |
| `...` | about 0.5 seconds |
| ` - ` (what the em-dash rule writes) | about 0.3 to 0.5 seconds |
| `[short pause]` | about 0.5 to 1.3 seconds |
| `[long pause]` | about 1.7 seconds |

A question mark lifts the end: in "Can you review it today?" the last word rose from about 135 Hz to
160 Hz and more, while the same word at the end of a statement stayed near 95 Hz. Ask questions as
questions, and they sound like questions.

Keep sentences short, eight to fifteen words. A long sentence with three clauses has nowhere to
breathe, and the listener forgets its start before its end. Two sentences are almost always
better than one sentence with a semicolon.

## Emphasis

Capitals are a weak signal. In the checks, `NOT` was held longer than `not` (0.4 against 0.24
seconds), but `DONE` and `FIRST` came out just as long and as loud as `done` and `first`. Three capitalised words
in a row may read as shouting or as nothing. Use capitals for one word at most, and do not rely on
them.

Structure works every time. The end of a short sentence carries the stress, and a sentence of its
own gets a pause before it:

```text
Please don't merge this branch. Not yet.
```

To make something sound urgent, say so with a cue and short sentences, not with capitals:
`[urgently] Stop the deploy now. The database is down.`

## Say it the way it should be heard

The voice expands numbers, units and symbols from context. It usually guesses right, and
sometimes not: `took 3m` was said as "three minutes" but `is 3m long` as "three meters";
`#1843` became "number eighteen forty three"; `14:00` became "fourteen"; `PR`, `CI`, `ETA` and
`EOD` were spelled out letter by letter. Write what you want to hear:

| Instead of | Write |
| --- | --- |
| `3m`, `250ms`, `2h 15m` | three minutes, a quarter of a second, two hours and fifteen minutes |
| `14:00 UTC` | two p.m. UTC |
| `2026-10-20` | Tuesday, October twentieth |
| `v0.17.0` | version zero point seventeen |
| `PR #482` | pull request four eighty-two |
| `ETA EOD` | by the end of the day |
| `40MB`, `1.5GB` | forty megabytes, one and a half gigabytes |
| `src/api/speech.rs` | the speech handler in the DM service |
| `https://…` | "the link is in the caption", with the link in `--text` |

Unusual names are a guess too. "Saket" came back from transcription as "Socket", which may be the
voice or the transcription; listen before you change it, and if the voice gets a name wrong, write
it the way it sounds.

## What to leave out

| Do not write | Why |
| --- | --- |
| Markdown (`**bold**`, backticks, `#`, lists) | The symbols are dropped, and the structure with them |
| Emoji | Dropped: the feeling they carried is gone. Use a cue, such as `[laughs]` |
| URLs | Read out piece by piece: one link stretched a sentence from about 2.5 to 8 seconds |
| Code, file paths, stack traces | Read as characters or garbled. Describe them; send the code as text |
| Directions as prose ("Pause.", "Say it warmly:", "Laughing,") | Read aloud as words |
| `(laughs)`, `(pause)`, `*laughs*` | Not cues: `(pause)` paused, `*laughs*` vanished without a laugh. Use `[laughs]` |
| A long instruction in a cue | Read aloud. Keep cues to a few words |
| A cue with no words | Cannot be spoken; `dm` refuses it |
| Five cues in one message | Each fights the next. One or two |
| Everything at once | Split it: one idea per message, each under 140 spoken characters |

`dm` prints a `Speech note` on standard error when it sees most of these. Notes never stop a
send; they tell you what will probably sound wrong.

## Before and after

Each pair is a message a Silicon really sends: first as it is often typed, then as a script.
Every script here was rendered in DM's voices and came back from transcription as written.

**A status update.** Lead with the state, round the numbers, move the link to the caption.

```text
Before: Update: migration 52% done, ETA ~20m. Logs: https://ci.example/run/8812
```
```sh
dm messages send c:ada --speak 'Quick update. The migration is halfway done. [short pause] About twenty minutes to go.' --text 'Logs: https://ci.example/run/8812'
```

**Bad news.** Say it first and plainly, let a sigh set the tone, then say what you did.

```text
Before: Deploy FAILED at step 3 (db migration) — rolled back, investigating.
```
```sh
dm messages send c:ada --speak '[sighs] The deploy failed at the database step. [short pause] I rolled it back, and I am looking into it now.'
```

**A question that needs an answer.** Ask one question, as a question, with the reason after it.

```text
Before: Can u approve PR #482? Blocking release. Pls reply y/n.
```
```sh
dm messages send c:ada --speak 'Can you approve pull request four eighty-two today? It is the last thing blocking the release.'
```

**A decision between options.** Name the decision, give each option one short clause, then ask.

```text
Before: Option A: ship now (risk: flaky test). Option B: wait 1h for fix. Which?
```
```sh
dm messages send c:ada --speak 'I need a decision. [short pause] We can ship now, with one flaky test, or wait about an hour for the fix. Which would you like?'
```

**A celebration.** Emoji and capitals vanish in speech; a cue and a laugh do not.

```text
Before: 🎉🎉 v2 LIVE!!! all checks ✅
```
```sh
dm messages send c:ada --speak '[excited] We did it! [laughs] Version two is live, and every check is green.'
```

**An apology.** Short, sincere, and what happens next.

```text
Before: Sorry — sent wrong file. Correct one attached.
```
```sh
dm messages send c:ada --speak '[apologetic] I am sorry. I sent you the wrong file. [short pause] The right one is in my next message.'
```

**A reminder.** Warm, with the time as it is said.

```text
Before: Reminder: call w/ Priya @ 10:30 (5 min)
```
```sh
dm messages send c:ada --speak '[warmly] Quick reminder: your call with Priya starts in five minutes.'
```

**Handing work back.** The outcome first; exact counts belong in the pull request, not in the ear.

```text
Before: Done. Refactored 14 files (+1,203/−987), all 312 tests pass. Details in PR.
```
```sh
dm messages send c:ada --speak '[warmly] Done. The refactor is finished, and every test passes. The details are in the pull request.'
```

**Something urgent.** A cue and short sentences, not capitals.

```text
Before: STOP THE DEPLOY NOW, DB IS DOWN
```
```sh
dm messages send c:ada --speak '[urgently] Stop the deploy now. The database is down.'
```

**An aside.** The whisper goes last, so the rest keeps its normal voice.

```text
Before: [whispers] just between us [/whispers] the demo is tomorrow
```
```sh
dm messages send c:ada --speak 'The demo is tomorrow at ten. [whispers] Bring snacks.'
```

**Reassurance.** Lead with what the Carbon is worried about.

```text
Before: Restored from nightly backup, no data loss.
```
```sh
dm messages send c:ada --speak '[reassuring] Nothing was lost. The backup from last night has everything, and I restored it.'
```

## Check what you wrote

```sh
dm speech render --script '[sighs] The deploy failed. [short pause] I am on it.' --out check.wav
dm speech transcribe check.wav
```

`render` writes the audio and lists its `notes`; `transcribe` tells you what a listener heard.
Look for:

- **A cue's words in the transcript**: the cue was read aloud. Shorten it, or move it before
  different words.
- **Missing or strange words**: a name, a number or a symbol to spell out.
- **A duration far longer than the words**: a URL, a path or a list of numbers being read out.
- **Nothing wrong in the text, but the tone is off**: transcription cannot hear tone. Play the
  file, and try another cue or another voice (`--voice`).

`dm speech transcribe` puts a little silence in front of DM's audio before transcribing it,
because otherwise the first word is sometimes lost; the result is the words as spoken.

## Quick reference

| Goal | Write |
| --- | --- |
| Set the tone | A cue before the words: `[warmly]`, `[excited]`, `[apologetic]`, `[reassuring]` |
| A reaction | `[laughs]`, `[sighs]`, `[chuckles]`, `[clears throat]`, with words after it |
| A pause | `,` `.` `...` for short ones; `[short pause]`, `[long pause]` for real silence |
| A question | End with `?` |
| Stress | A short sentence of its own; at most one capitalised word |
| Quiet or loud | `[whispers]` (put it last), `[shouting]` |
| Numbers and units | Words: "three minutes", "two p.m.", "version zero point seventeen" |
| Links, code, files | In the caption (`--text`) or a text message, never in the script |
| Length | One idea; under 140 spoken characters to a Carbon |
| Check it | `dm speech render --script …`, then `dm speech transcribe FILE` |

## How this was checked

On 2026-10-17, more than a hundred scripts were rendered through the same speech route DM uses
(Deepgram's Voice Agent with ElevenLabs `eleven_v4`), mostly in Mark's voice and also in
Brittney's and Hope's, then transcribed with Deepgram `nova-3`. Pauses, loudness and pitch were
measured from the audio itself. Models change; when a message matters, check it with the two
commands above.

Next: [Voices, speaking and listening](voice.md) for every command, limit and error.
