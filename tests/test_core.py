"""Tests for the fiddly parts that are easy to break. Run:  .venv\\Scripts\\python -m pytest -q"""
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from streamtranslator import chat, detect, snap, sync, timefmt, translate  # noqa: E402
from streamtranslator.glossary import Glossary  # noqa: E402
from streamtranslator.pipeline import (Segment, VodInfo, _balanced_wrap, read_srt, to_cues,  # noqa: E402
                                       write_srt_cues)


# ---------------------------------------------------------------- time
@pytest.mark.parametrize("text,secs", [("1:02:30", 3750), ("62:30", 3750), ("3750", 3750), ("1h2m30s", 3750),
                                       ("45m", 2700), ("0:05", 5)])
def test_parse_ts(text, secs):
    assert timefmt.parse_ts(text) == secs


@pytest.mark.parametrize("bad", ["", "1::2", "abc", "1:2:3:4"])
def test_parse_ts_rejects(bad):
    with pytest.raises(ValueError):
        timefmt.parse_ts(bad)


def test_format_and_tags():
    assert timefmt.format_ts(3750) == "1:02:30"
    assert timefmt.format_ts(95) == "1:35"
    assert timefmt.format_hms_tag(3750) == "1h02m30s"
    assert timefmt.srt_ts(3750.5) == "01:02:30,500"


# ---------------------------------------------------------------- glossary
@pytest.fixture(scope="module")
def gl():
    g = Glossary.get()
    g.enabled = True
    return g


@pytest.mark.parametrize("src,want", [
    ("Nafiri flashes in", "Naafiri flashes in"),
    ("Casante dies", "K'Sante dies"),
    ("they take the Heraldo", "they take the Herald"),
    ("Kaisa is fed", "Kai'Sa is fed"),
    ("Lee Shin goes in", "Lee Sin goes in"),
    ("the Void Larvae", "the Void Grubs"),
    ("He used the ulti", "He used the ult"),
])
def test_glossary_fixes(gl, src, want):
    assert gl.fix(src)[0] == want


@pytest.mark.parametrize("text", ["There is a brand new strategy", "Thank you Victor", "Graves and Viktor are strong"])
def test_glossary_leaves_english_alone(gl, text):
    assert gl.fix(text)[0] == text


def test_glossary_extra_names(gl):
    assert gl.fix("Fakker played well", extra=["Faker"])[0] == "Faker played well"


def test_hotwords_without_core_terms(gl):
    hw = gl.hotwords(["Orianna"], core=False, extra=["Faker"])
    assert hw.startswith("Orianna, Faker") and "Rift Herald" not in hw


def test_prompt_echo(gl):
    hw = gl.hotwords(["Orianna", "Nocturne"])
    assert gl.is_prompt_echo("Orianna, Nocturne, Baron, Dragon, Elder Dragon", hw)
    assert not gl.is_prompt_echo("Orianna goes for the Baron, nice", hw)


# ---------------------------------------------------------------- chat filters
@pytest.mark.parametrize("src,want", [
    ("JAJAJAJAJA", ""), ("JSJWKSKKAKAKA", ""), ("jajaja que malo es", "que malo es"), ("kkkkkk mano", "mano"),
    ("rsrsrs que isso", "que isso"), ("hijo de su madre", "hijo de su madre"), ("esas hojas", "esas hojas"),
    ("ahi esta", "ahi esta"), ("Jaime ganó", "Jaime ganó"),
])
def test_strip_laughs(src, want):
    assert chat.strip_laughs(src) == want


def test_is_english():
    assert chat.is_english("lol that was so good bro")
    assert not chat.is_english("que jugada mas buena hermano")


# ---------------------------------------------------------------- sync
@pytest.mark.parametrize("text,kind", [
    ("ibai", "channel"), ("twitch.tv/caedrel", "channel"), ("2923514362", "vod"),
    ("https://www.twitch.tv/videos/2923514362?t=1h", "vod"), ("https://youtube.com/watch?v=x", "url"),
])
def test_classify(text, kind):
    assert sync.classify(text)[0] == kind


def _vod(id_, start_ts, duration):
    return VodInfo(url=f"https://www.twitch.tv/videos/{id_}", id=id_, title="t", channel=f"ch{id_}",
                   duration=duration, streams={"Source": "x"}, preview_stream="x", start_ts=start_ts)


def test_sync_maps_times(monkeypatch):
    mine = _vod("1", start_ts=1_000_000, duration=4 * 3600)
    theirs = _vod("2", start_ts=1_000_000 - 600, duration=5 * 3600)       # they went live 10 min earlier
    monkeypatch.setattr(sync, "fetch_info", lambda _url: theirs)
    r = sync.match(mine, 3600, 3700, "https://www.twitch.tv/videos/2")
    assert r["offset"] == 600 and r["start"] == 4200 and r["end"] == 4300 and not r["clipped"]


def test_sync_rejects_uncovered_moment(monkeypatch):
    mine = _vod("1", start_ts=1_000_000, duration=4 * 3600)
    theirs = _vod("2", start_ts=1_000_000 + 7200, duration=600)           # live later, briefly
    monkeypatch.setattr(sync, "fetch_info", lambda _url: theirs)
    with pytest.raises(Exception):
        sync.match(mine, 60, 120, "https://www.twitch.tv/videos/2")


def test_vod_covering():
    vods = [{"start_ts": 1000.0, "duration": 500.0}, {"start_ts": 5000.0, "duration": 500.0}]
    assert sync._vod_covering(vods, 5200) is vods[1]
    assert sync._vod_covering(vods, 3000) is None


# ---------------------------------------------------------------- snap (game clock)
def test_clock_values_fix_ocr_letters():
    assert snap.clock_values(["• 15:12", "1:37", "138.339"]) == [912, 97]
    assert snap.clock_values(["2O:O5"]) == [1205]
    assert snap.clock_values(["1:02:30"]) == []            # h:mm:ss is stream uptime, not the game clock


def test_minute_variants_cover_misreads():
    # "27:22" read as "21:22": the true value must be among the variants
    assert 27 * 60 + 22 in snap._minute_variants(21 * 60 + 22)
    assert 26 * 60 + 22 in snap._minute_variants(28 * 60 + 22)


# ---------------------------------------------------------------- translate / subtitles
def test_split_sentences():
    assert translate.split_sentences("Ya, ya. No, literal.") == ["Ya, ya.", "No, literal."]
    assert translate.split_sentences("one line") == ["one line"]


def test_balanced_wrap():
    out = _balanced_wrap("They're fighting 5v4. Heraldo is at 6,000 HP.", 42)
    a, b = out.split("\n")
    assert abs(len(a) - len(b)) < 12


def test_cues_split_long_segments():
    cues = to_cues([Segment(0, 10, "word " * 60)])
    assert len(cues) > 1 and all(c.text.count("\n") <= 1 for c in cues)
    assert cues[0].start == 0 and abs(cues[-1].end - 10) < 0.01


def test_srt_roundtrip(tmp_path):
    cues = [{"start": 1.0, "end": 2.5, "text": "Hello"}, {"start": 0.2, "end": 0.9, "text": "First"},
            {"start": 3, "end": 4, "text": "   "}]
    p = tmp_path / "x.srt"
    write_srt_cues(cues, p)
    back = read_srt(p)
    assert [c["text"] for c in back] == ["First", "Hello"]            # sorted, blank line dropped
    assert back[1]["start"] == 1.0 and back[1]["end"] == 2.5


# ---------------------------------------------------------------- detection
def test_players_from_ocr():
    frames = [["Tl Faker", "BLG Knight", "Xun", "verizon", "NEARSIGHTED", "LCK#3"],
              ["T1 Faker", "Xun", "BLG_Knight", "PATCH 26.20"],
              ["Tl_oner", "Xun", "Faker"]]
    names = detect.players_in(frames)
    assert "Faker" in names and "Knight" in names and "Xun" in names
    assert not {"Verizon", "Nearsighted", "Patch", "Lck"} & set(names)
