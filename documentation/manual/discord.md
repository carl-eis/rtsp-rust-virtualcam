# The Discord tab

RTSP Cam can run a **Discord bot** that sends the sound of an audio device (a microphone, a
virtual cable such as VB-Cable, a line input, or on Windows what the PC itself plays) into a
voice channel, and can play the channel back on your speakers. It lives in the **Discord** tab
of the window. Nothing about it runs until you log in: the cameras work exactly the same
whether you use it or not.

The bot needs its own Discord account (a *bot account*). It can't use your personal account:
that is against Discord's terms.

## 1. Create the bot (once)

1. Open the [Discord Developer Portal](https://discord.com/developers/applications) and sign
   in. Click **New Application**, give it a name (this is the bot's name in channels), accept
   the terms and click **Create**.
2. **Make it private**, so nobody else can add it to their server:
   - On the **Installation** page, untick **User Install**, and set **Install Link** to
     **None**. Save.
   - On the **Bot** page, untick **Public Bot**. Save.
3. On the **Bot** page, click **Reset Token**, confirm, and **Copy** the token. Keep it
   secret: anyone with it controls the bot. (If it leaks, reset it here; the old one stops
   working.)
4. The bot needs **no privileged intents**. Leave *Presence*, *Server Members* and *Message
   Content* off.

## 2. Log in and invite it

1. In RTSP Cam, open the **Discord** tab, paste the token into **Bot token** (or click
   **Paste**), and click **Log in**. The status line shows the bot's name when it's online.
   The token is saved encrypted (like stream passwords) in `discord.json`, next to
   `config.json`.
2. Click **Invite to a server**. Your browser opens Discord's invite page; pick the server and
   click **Authorize**. The bot asks only for *View Channels*, *Connect* and *Speak*. You need
   the *Manage Server* permission on that server to add a bot.
3. The server appears in the **Server** list within a few seconds.

Tick **Log in when RTSP Cam starts** to have the bot come online with the app (also when the
app runs with `--headless`). What it joins after logging in is set per server (section 5);
with nothing set, it joins the channel it was last in, unless you left it with **Leave**.

## 3. Send sound

1. Under **Sound**, pick the **Source**. Click **Refresh** if a device you just plugged in is
   missing. On Windows the list also has the output devices, marked *what the PC plays*: they
   record what that output plays (a game, music), with no virtual cable needed. On Linux the
   same comes from the *Monitor of ...* inputs that PipeWire and PulseAudio list.
2. Under **Voice**, pick the **Server** and **Channel**, and click **Join**. A channel marked
   *no permission to speak* needs the bot's role to have *Connect* and *Speak* there.
3. The bot joins **muted**. Watch the **Level** meter, then click **Unmute (start sending)**.
   The tray menu has **Discord: Mute / Unmute** while the bot is in a channel.

The **noise gate** sends nothing while the sound stays below the threshold (the red mark on the
meter), so the room's background noise doesn't reach the channel. The meter turns green when
sound gets through. Drag the slider until your normal voice is green and silence is grey.

Live sound stays live: if sending falls behind by more than 200 ms (a slow network, a busy PC),
the oldest sound is skipped. The voice line under the channel says when that happens.

## 4. Hear the channel

Under **Hear the channel**, tick **Play the channel on** and pick an output (speakers or a
headset). While the bot is in a channel, everyone talking there is mixed and played on that
output; the line below says how many people are talking. **Volume** goes up to 200 %. It is
off by default, and the output is only opened while the bot is in a channel.

The bot doesn't hear itself: Discord doesn't send a bot's own sound back to it.

This plays (and lets you record) what everyone in the channel says. Depending on where you
live, recording people may need their consent.

## 5. Join on its own: auto-join and follow

The two pickers under the channel apply to the **server picked above**, and each server keeps
its own:

- **After logging in**: a channel the bot joins when it logs in.
- **Follow**: a person, from the people in the server's voice channels. When they join, move
  to or leave a voice channel in that server, the bot does the same (it stays logged in when
  it leaves). The bot can see this without any privileged intent.

The bot is in one channel at a time, so after logging in it picks, in this order: a followed
person who is already in a voice channel, then a server's **After logging in** channel, then
the last channel. Following someone moves the bot even if it was somewhere else. To follow
someone who isn't in voice right now, put their user id in `discord.json` (see section 7).

**Stage channels.** In a stage channel the bot first tries to become a speaker, which needs
the *Mute Members* permission there. Without it, it asks to speak, and the voice line says
it's waiting: a stage moderator accepts it from the stage's audience list. Until then nobody
hears it.

**Leave this server** (next to the server picker) takes the bot out of a server, after asking.
It needs a new invite to come back.

## 6. When something goes wrong

| What the tab says | What to do |
|---|---|
| *Discord refused the token* | Copy the token again from the Bot page (**Reset Token**). |
| *Joining the voice channel timed out* | Check that the bot's role can see the channel, connect and speak there, and that the channel isn't full. |
| *... encryption (close code 4017)* | Discord changed its voice encryption (DAVE) and this version of RTSP Cam can't follow. Update RTSP Cam. |
| *The bot was disconnected from the voice channel* | Someone disconnected it, or the channel was deleted. Join again. |
| *The connection ... dropped; trying again* | Nothing: the bot rejoins by itself (after 1, 2, 5, 10, then every 30 seconds). |
| *The sound source failed* | The device was unplugged or taken by another program. Plug it back in or pick another source; the bot tries again every 3 seconds. |
| *The output for hearing the channel failed* | The same, for the output under **Hear the channel**. |
| *The sound could not be sent* | The voice connection couldn't play the sound; the bot joins again by itself. If it keeps happening, report it with the log. |
| *Could not ask to speak on the stage* | Give the bot's role *Request to Speak* (or *Mute Members*) on the stage, or have a moderator invite it. |

The log (**Help > Open logs folder**) has the details. The token never appears in it.

## 7. Notes

- One bot can be in one voice channel at a time from this tab.
- A camera's own sound as the source is planned (documentation/19, phase D7).
- `discord.json` can be edited by hand while the app runs; changes to the source, mute, gate,
  hearing the channel, the per-server rules and the token apply at once. The per-server rules
  look like this (ids are the ones Discord shows with *Developer Mode* on, under *Settings >
  Advanced*: right-click a server, channel or person, *Copy ID*):

  ```json
  "servers": {
    "123456789012345678": {
      "auto_join_channel": "234567890123456789",
      "follow_user": "345678901234567890"
    }
  }
  ```
