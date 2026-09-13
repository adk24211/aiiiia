# Licensing

hookline is MIT. See [`LICENSE`](../LICENSE).

MIT is the right default here: it is short, universally understood, and passes
a legal review without a meeting. For a project whose job is to be adopted,
anything that adds a meeting is a cost.

## If you want to change it

Two situations lead people away from MIT, and they want different answers.

**"A competitor could host this and contribute nothing."** The usual answer is
**AGPL-3.0**: the software stays free software, and anyone offering it as a
network service has to publish their modifications. It does not stop a
competitor hosting it; it stops them doing so privately. Note that many
companies' internal policies refuse AGPL dependencies outright, so this costs
adoption among exactly the large customers you might want.

**"I want to sell a hosted version and nobody else should."** The usual answer
is a source-available licence, most commonly **BSL 1.1** with a change date:
the source is public, anyone may read, modify and self-host it, offering it as
a competing service is forbidden, and each release becomes open source (you
pick the licence it converts to) after a fixed period, typically four years.
BSL is not an OSI-approved open source licence and should not be described as
one.

A third arrangement, and often the best of them: keep the core MIT and put
only the commercial features under a separate licence in a separate directory
or repository. That is what [`commercial.md`](commercial.md) assumes.

## Doing it

1. **Get agreement from every contributor.** You cannot relicense someone
   else's copyrighted contribution without their permission. This is trivial
   while the contributor list is one person and becomes a research project
   later, so decide early.
2. **Require a CLA or a DCO** from outside contributors if you expect to
   relicense at any point. Without one, every merged pull request narrows your
   options permanently.
3. **Use the canonical text.** Copy the licence verbatim from its source —
   `gnu.org` for AGPL-3.0, `mariadb.com/bsl11` for BSL 1.1 — and fill in only
   the parameters the licence tells you to. Do not retype it, do not summarise
   it, and do not let anyone paraphrase it for you, this document included.
4. **Get it read by a lawyer** before you rely on it. The paragraphs above are
   a description of what these licences are generally understood to do, not
   legal advice, and the difference matters when it matters.

## Third-party licences

hookline's dependencies are all MIT or Apache-2.0, which impose no obligation
beyond preserving the notices. `cargo tree` lists them; a distributor should
ship the notices with any binary.
