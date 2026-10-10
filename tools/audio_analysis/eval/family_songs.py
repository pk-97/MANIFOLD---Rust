"""Songs a melodic family can be labelled on: every song with a project and a proven export offset (kick goal songs
through kick_goal_melodic._source, plus the proven 2024/2025 WIPs) whose project has notes or audio clips on the
family's tracks. Cached per family in GOAL/{family}_songs.json (projects read only through als_extract)."""
import json


def family_songs(fam):
    from tools.audio_analysis.eval.als_extract import extract
    from tools.audio_analysis.eval.detector_songs import wip
    from tools.audio_analysis.eval.kick_goal_eval import GOAL, Goal
    from tools.audio_analysis.eval.kick_goal_melodic import _source
    path = GOAL / f'{fam.name}_songs.json'
    if path.exists():
        return tuple(json.loads(path.read_text()))
    g = Goal(mode='v3')
    W = wip()
    out = []
    for t in [t for t in g.records if t not in W] + list(W):
        src = (W[t]['als'], W[t]['offset_s']) if t in W else _source(t)
        if not src:
            continue
        res = extract(src[0])
        notes = any(c['notes'] for tr in res['tracks'] if fam.voice.search(tr['name']) and not fam.exclude.search(tr['name'])
                    for c in tr['midi_clips'])
        clips = any(fam.voice.search(c.get('track') or '') and not fam.exclude.search(c.get('track') or '') for c in res['audio_clips'])
        if notes or clips:
            out.append(t)
    path.write_text(json.dumps(out))
    return tuple(out)
