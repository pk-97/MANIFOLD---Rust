"""What part a project track plays, from the names Peter gives things.

Tracks are often named after the plugin or preset ("28-Diva", "808 Drifter" used as a lead) while the group says
what they are ("27 Subs", "14 Leads"), so a track is classed by its groups from the innermost outward, then by its
own name. A drum-rack pad is classed by its own name, then by its track. Leading track numbers ("28-", "40 ") are ignored. The first class
whose pattern matches wins, in the order below; '?' means nothing matched (plugin-named tracks outside any group).
Resolume and trigger tracks drive visuals, not sound: class 'control'. Frozen, flattened and bounced audio keeps its track's
class: a flattened clap track holds claps; only unclassed audio may hold anything.
"""
import re

CLASSES = (
    ('control', re.compile(r'resolume|trigger', re.I)),
    ('drums', re.compile(r'kick|snare|\bsnr|clap|hat|\bhh\b|ride|cymbal|crash|perc|shaker|tamb|\btom|rim|drum|break|beat|top ?loop|groove|conga|bongo|linn', re.I)),
    ('bass', re.compile(r'bass|\bsubs?\b|growl|reese|wobble|neuro|\b808|donk|screech|riddim|low ?end', re.I)),
    ('vocal', re.compile(r'vox|vocal|voice|choir|chant|acapella|\bvoc\b', re.I)),
    ('fx', re.compile(r'\bfx|_fx_|riser|uplifter|downlifter|sweep|impact|noise|transition|whoosh|reverse|atmos|ambien|texture|foley|sfx|'
                      r'forest|rain|street|city|water|ocean|wave|crowd|clock|vinyl|crackle|radio|broadcast|breath|sonar|birds|wind|field|ear tickle', re.I)),
    ('pad', re.compile(r'\bpads?\b|drone|string|swell', re.I)),
    ('synth', re.compile(r'synth|lead|arp|pluck|stab|chord|key|piano|rhodes|organ|bell|mallet|marimba|saw|square|poly|hook|melod|seq|guitar|flute|brass|horn', re.I)),
)


def classify(name, groups=(), file=''):
    """The part a track (or an audio clip: its file name is the last fallback) plays."""
    for text in (*groups, name, file):
        text = re.sub(r'^\d+[- ]', '', text or '')
        for cls, rx in CLASSES:
            if rx.search(text):
                return cls
    return '?'
