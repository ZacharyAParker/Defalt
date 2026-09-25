# Terms of use

Effective September 25, 2026 (terms version 2026-09-25). Applies to Defalt 0.4.4 as distributed in this repository.

## Agreement

These terms are an agreement between you and Zachary Parker, who maintains Defalt ("the maintainer"). They cover the Defalt console, the radio backend, the browser and phone player, and the documentation in this repository (together, "Defalt").

By installing, building, running or otherwise using Defalt, you agree to these terms, the LICENSE and the privacy policy (PRIVACY.md). The first time you open the console or the browser player, and again whenever these terms change, Defalt asks you to accept them before anything plays. If you don't agree, don't use Defalt.

You must be old enough to form a binding agreement where you live, or have a parent or guardian agree for you. Defalt plays explicit music and mature comedy and is meant for adults.

## License

Defalt is source-available, not open source. All rights are reserved. The LICENSE file lets you view the source on GitHub and build and run Defalt on devices you own or control, for your own personal, noncommercial use. It doesn't let you copy, modify, redistribute, sublicense or commercially use Defalt without written permission. Read LICENSE for the exact wording; if LICENSE and these terms conflict about software rights, LICENSE controls.

Third-party components keep their own licenses. See THIRD-PARTY-NOTICES.md and vendor/README.md.

## Your content and third-party services

Defalt doesn't include music. You choose what it plays, downloads, looks up and sends to other services, and you are solely responsible for having the rights or permission to do so. That includes music, recordings, lyrics, artwork, articles, voices and anything else you submit or request.

A streaming subscription does not by itself give you permission to obtain recordings from another source or to redistribute them. Some sources Defalt can use may prohibit downloading in their own terms. Check the rules that apply to you before using those features, or point the station at your own files instead.

When you enable a feature that uses an outside service, you deal with that service directly and must follow its terms, policies and usage limits. Depending on what you turn on, these include:

- YouTube and YouTube Music (searches, metadata and audio sources)
- Spotify (catalog search, metadata and artwork)
- Apple Music and iTunes charts, and Kworb (public chart pages)
- Steam (library, wishlist and news)
- OpenRouter and the language-model and voice providers it routes to, the Codex CLI and its provider, and Microsoft Edge text-to-speech
- Wikipedia (song background)
- LRCLIB (synced lyrics)
- Open-Meteo (weather)
- Cloudflare, including Cloudflare Tunnel and Cloudflare Access (remote listening)
- RSS feeds and the websites of articles you submit

You are responsible for your accounts, API keys and credentials with those services, and for any charges they bill you. Don't use Defalt to bypass access controls, paywalls, digital rights management, rate limits or technical protection measures, or to infringe anyone's copyright, trademark, privacy or other rights.

Keep it personal. Defalt is not a public streaming, broadcasting or redistribution service. Don't share, upload, sell or publicly perform the audio it caches or plays, and don't run it as a service for other people.

## Remote listening

The local server has no login of its own. Don't put it on a public address or forward a port to it. The only supported way to reach Defalt from somewhere else is remote listening through your own Cloudflare Tunnel, with a Cloudflare Access application in front of it, as described in docs/REMOTE.md. The station also checks the Access token on every remote request.

If you set up remote listening, you are responsible for how the tunnel, domain and Access policy are configured, for keeping that policy limited to yourself, and for anyone you let in. Remote listening is for your own devices. Letting other people listen can turn a personal copy into a public performance or distribution that you don't have rights to.

## Generated content

Host dialogue, jokes, roasts, synthetic voices, news and article summaries, song background, request interpretation, discovery picks and fictional ads are produced automatically by language models, speech providers and code. They can be wrong, out of date, offensive, repetitive or incomplete, and lyrics lookups can return the wrong song.

Generated content is not a statement by the maintainer or by the artists, publishers, companies or people it mentions, and it is not professional, legal, medical, financial or safety advice. Check original sources before relying on a factual claim. The synthetic hosts are fictional characters, not real people.

The ads are comedy. Nothing on the station is sponsored, nobody is paid, and a fictional ad is not an endorsement of, or an offer from, any product, game, company or person.

## Explicit and mature content

Defalt prefers the explicit version of a song when one exists, and the hosts are written to roast your picks and joke about news, games and pop culture. Expect strong language and mature themes. Don't use Defalt where that would be inappropriate, and think about who else can hear it.

## Health and safety

Photosensitivity warning: the radio booth has flashing and flickering animation, including lightning flashes, a flickering neon ON AIR sign, flickering city windows and a beat-driven visualizer. A small number of people can have seizures triggered by flashing lights or patterns, even with no history of epilepsy. If you or anyone watching has had seizures or epilepsy, talk to a doctor before use. Stop immediately and seek medical attention if you notice dizziness, altered vision, eye or muscle twitching, loss of awareness, disorientation, involuntary movements or convulsions.

To calm the booth, turn on Reduced motion (Studio settings on the radio view, in both the console and the browser player; you can also turn it on from the first-run notice). Reduced motion stops the lightning, the flicker and the fast animation. You can turn off Lightning on its own in the same menu. The browser player also follows your system's reduce-motion setting until you pick one.

Hearing safety: listening at high volume, especially for long periods or on headphones and earbuds, can permanently damage your hearing. Start with the volume low and keep it at a safe level. Transitions, effects and generated speech can change loudness suddenly. The master limiter is there to stop digital clipping. It is not a hearing-protection device and doesn't guarantee a safe listening level.

You are responsible for your audio equipment and your environment. Don't use Defalt in a way that distracts you while driving, cycling or doing anything else that needs your attention; set up playback before you set off and follow local laws on phone use.

## Your data and backups

Defalt stores its database, caches, settings, logs and optional vault on your machine; see PRIVACY.md for what leaves it. Updates, bugs, crashes and cache settings can delete or damage local files, including downloaded audio, library metadata and preferences. Keep backups of anything you value. The maintainer doesn't hold a copy of your data and can't restore it.

## No warranty

DEFALT IS PROVIDED "AS IS" AND "AS AVAILABLE", WITH ALL FAULTS AND WITHOUT WARRANTY OF ANY KIND. TO THE MAXIMUM EXTENT PERMITTED BY APPLICABLE LAW, THE MAINTAINER DISCLAIMS ALL WARRANTIES, EXPRESS, IMPLIED OR STATUTORY, INCLUDING ANY WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE, TITLE, NON-INFRINGEMENT, ACCURACY AND QUIET ENJOYMENT, AND ANY WARRANTIES ARISING FROM COURSE OF DEALING OR USAGE OF TRADE. THE MAINTAINER DOES NOT WARRANT THAT DEFALT WILL BE UNINTERRUPTED, SECURE OR ERROR-FREE, THAT IT WILL MATCH THE RIGHT SONG OR VERSION, THAT GENERATED CONTENT WILL BE ACCURATE OR APPROPRIATE, THAT IT WILL WORK WITH YOUR DEVICES, OR THAT ANY THIRD-PARTY SERVICE WILL REMAIN AVAILABLE. YOU USE DEFALT AT YOUR OWN RISK.

## Limitation of liability

TO THE MAXIMUM EXTENT PERMITTED BY APPLICABLE LAW, IN NO EVENT WILL THE MAINTAINER OR ANY CONTRIBUTOR BE LIABLE FOR ANY INDIRECT, INCIDENTAL, SPECIAL, CONSEQUENTIAL, EXEMPLARY OR PUNITIVE DAMAGES, OR FOR ANY LOSS OF DATA, PROFITS, REVENUE, GOODWILL OR USE, HEARING LOSS OR OTHER PERSONAL INJURY, DAMAGE TO EQUIPMENT, SERVICE CHARGES, ACCOUNT SUSPENSIONS, OR CLAIMS BY THIRD PARTIES, ARISING OUT OF OR RELATED TO DEFALT OR THESE TERMS, HOWEVER CAUSED AND UNDER ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, TORT (INCLUDING NEGLIGENCE), STRICT LIABILITY OR OTHERWISE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGES.

TO THE MAXIMUM EXTENT PERMITTED BY APPLICABLE LAW, THE MAINTAINER'S TOTAL LIABILITY FOR ALL CLAIMS ARISING OUT OF OR RELATED TO DEFALT OR THESE TERMS IS LIMITED TO THE AMOUNT YOU PAID THE MAINTAINER FOR DEFALT. DEFALT IS PROVIDED FREE OF CHARGE, SO THAT AMOUNT IS ZERO (US$0).

SOME JURISDICTIONS DO NOT ALLOW THE EXCLUSION OF CERTAIN WARRANTIES OR THE LIMITATION OR EXCLUSION OF LIABILITY FOR CERTAIN DAMAGES, SUCH AS DEATH OR PERSONAL INJURY CAUSED BY NEGLIGENCE, FRAUD, OR GROSS NEGLIGENCE OR WILLFUL MISCONDUCT. IN THOSE JURISDICTIONS, THE ABOVE EXCLUSIONS AND LIMITATIONS APPLY ONLY TO THE EXTENT THE LAW ALLOWS, AND NOTHING IN THESE TERMS EXCLUDES OR LIMITS LIABILITY OR RIGHTS THAT CANNOT BE EXCLUDED OR LIMITED UNDER APPLICABLE LAW.

## Indemnification

To the extent permitted by applicable law, you agree to defend, indemnify and hold harmless the maintainer and any contributors from and against any claims, demands, losses, damages, liabilities, costs and expenses, including reasonable attorneys' fees, arising out of or related to: your use of Defalt; the music, recordings, articles and other material you download, play, submit or share with it; your violation of these terms or the LICENSE; your violation of any law or of the rights or terms of any third party or service; or how you configure remote listening, including your tunnel, domain and access policy and anyone you let in.

## Third-party services and trademarks

Defalt is an independent personal project. It is not affiliated with, sponsored by or endorsed by YouTube, Google, Spotify, Apple, Valve (Steam), OpenRouter, OpenAI, Microsoft, the Wikimedia Foundation (Wikipedia), LRCLIB, Open-Meteo, Cloudflare, Kworb, Obsidian, or any artist, label, publisher, game studio or other company named in the app, its documentation or its generated content. The same goes for DJ software such as djay, rekordbox and Serato, which the documentation mentions only for comparison.

Those names and logos are trademarks of their respective owners and are used only to identify the services Defalt works with. Defalt doesn't control third-party services and isn't responsible for their availability, content, pricing, accuracy or changes to their terms.

## Termination

These terms apply for as long as you use Defalt. Your permission to use Defalt ends automatically if you break these terms or the LICENSE. You can stop at any time. When your permission ends or you stop, stop using Defalt and delete your copies of it, and delete cached audio you don't have rights to keep. The sections on your content, no warranty, limitation of liability, indemnification, governing law and general terms survive termination.

## Governing law and venue

These terms, and any dispute arising out of or related to them or to Defalt, are governed by the laws of the State of California, USA, without regard to its conflict-of-laws rules. To the extent permitted by applicable law, you and the maintainer agree to the exclusive jurisdiction and venue of the state and federal courts located in California for any such dispute. If you are a consumer, this doesn't take away any protection you have under the mandatory laws of the place you live.

## General

Severability: if any part of these terms is found unenforceable, it will be enforced to the maximum extent allowed and the rest of the terms stay in effect.

No waiver: if the maintainer doesn't enforce a part of these terms, that isn't a waiver of the right to enforce it later.

Entire agreement: these terms, the LICENSE and the privacy policy are the entire agreement between you and the maintainer about Defalt, and replace any earlier terms. Third-party components are governed by their own licenses.

Assignment: you can't transfer your rights under these terms without the maintainer's written permission. The maintainer may transfer them as part of transferring the project.

## Changes

Future releases may change these terms. Each version is identified by its effective date and the terms version at the top of this page. When the terms version changes, the console and the browser player ask you to accept the new terms before anything plays. Continuing to use Defalt after accepting the new terms means you agree to them. The version bundled with a native executable describes that build; the repository may describe a newer release.

## Contact

Contact the maintainer through the Defalt repository on GitHub. Report security problems privately as described in SECURITY.md. Don't post credentials, private listening records or other sensitive data in public issues.

---
Defalt v0.4.4 · © 2026 Zachary Parker
