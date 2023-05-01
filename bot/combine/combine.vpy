import math
from fractions import Fraction

import vapoursynth as vs
from vapoursynth import core
from havsfunc import ChangeFPS


MIN_FPS = 5
MAX_FPS = 60
MAX_HORIZONTAL_ASPECT_RATIO = 3


def blank(clip):
    return core.std.BlankClip(clip, color=[255], format=vs.GRAY8)


a_emoji = core.ffms2.Source("1f170.y4m")
b_emoji = core.ffms2.Source("1f171.y4m")

a = core.ffms2.Source(a)
b = core.ffms2.Source(b)

a = core.std.Crop(a, right=a.width % 2, top=a.height % 2)
b = core.std.Crop(b, right=b.width % 2, top=b.height % 2)

if len(a) == 1 and len(b) == 1:
    a = core.std.AssumeFPS(a, fpsnum=1, fpsden=1)
    b = core.std.AssumeFPS(b, fpsnum=1, fpsden=1)
elif len(a) == 1:
    a = core.std.AssumeFPS(a, b) * len(b)
elif len(b) == 1:
    b = core.std.AssumeFPS(b, a) * len(a)
else:
    larger_fps_rounded = round(max(a.fps, b.fps))
    test_framerates_lower = set()
    test_framerates_higher = set()
    for denom in range(1, 10):
        start_num = larger_fps_rounded * denom
        for num in range(round(0.6 * start_num), start_num):
            frac = Fraction(num, denom)
            if frac >= MIN_FPS and frac <= MAX_FPS:
                test_framerates_lower.add(frac)
        for num in range(start_num, round(1.8 * start_num)):
            frac = Fraction(num, denom)
            if frac >= MIN_FPS and frac <= MAX_FPS:
                test_framerates_higher.add(frac)

    best_frac = None
    best_score = None
    for frac in (
        max(a.fps, b.fps),
        *reversed(sorted(test_framerates_lower)),
        *sorted(test_framerates_higher),
    ):
        test_a = ChangeFPS(a, *frac.as_integer_ratio())
        test_b = ChangeFPS(b, *frac.as_integer_ratio())
        ratio = max(len(test_a), len(test_b)) / min(len(test_a), len(test_b))
        score = 1 - math.modf(ratio)[0]
        if best_score is None:
            best_score = score
            best_a = test_a
            best_b = test_b
        if score == 0:
            best_a = test_a
            best_b = test_b
            break
        elif score < best_score:
            best_score = score
            best_a = test_a
            best_b = test_b

    a = best_a
    b = best_b

    if len(a) < len(b):
        repeat = math.ceil(len(b) / len(a))
        a = (a * repeat)[: len(b)]
    else:
        repeat = math.ceil(len(a) / len(b))
        b = (b * repeat)[: len(a)]

a_horiz = a
b_horiz = b
if a.height < b.height:
    width = 2 * round(a.width * b.height / a.height / 2)
    a_horiz = core.resize.Lanczos(a, width, b.height)
elif a.height > b.height:
    width = 2 * round(b.width * a.height / b.height / 2)
    b_horiz = core.resize.Lanczos(b, width, a.height)
legend_height_horiz = 2 * round(a_horiz.height / 10)
width_horiz = a_horiz.width + b_horiz.width
height_horiz = a_horiz.height + legend_height_horiz
aspect_horiz = width_horiz / height_horiz


make_vertical = aspect_horiz > MAX_HORIZONTAL_ASPECT_RATIO

if make_vertical:
    if a.width < b.width:
        a = core.resize.Lanczos(a, b.width, 2 * round(a.height * b.width / a.width / 2))
    else:
        b = core.resize.Lanczos(b, a.width, 2 * round(b.height * a.width / b.width / 2))

    legend_height = 2 * round((a.height + b.height) / 15)

    a_height = a.height
    b_height = b.height

    a = core.std.AddBorders(a, top=legend_height, bottom=b_height + legend_height)

    b_mask = core.std.AddBorders(blank(b), top=a_height + 2 * legend_height)
    b = core.std.AddBorders(b, top=a_height + 2 * legend_height)

    merged = core.std.MaskedMerge(a, b, b_mask)

    emoji_size = 2 * round(0.8 * legend_height / 2)
    emoji_left = merged.width // 2 - emoji_size // 2
    emoji_left -= emoji_left % 2
    emoji_right = merged.width - emoji_left - emoji_size

    a_emoji_top = legend_height // 2 - emoji_size // 2
    a_emoji_top -= a_emoji_top % 2
    a_emoji_bottom = merged.height - a_emoji_top - emoji_size

    a_emoji = core.fmtc.bitdepth(
        core.fmtc.resample(a_emoji, emoji_size, emoji_size), bits=8
    )
    a_emoji_mask = core.std.AddBorders(
        blank(a_emoji),
        left=emoji_left,
        right=emoji_right,
        top=a_emoji_top,
        bottom=a_emoji_bottom,
    )
    a_emoji = core.std.AddBorders(
        a_emoji,
        left=emoji_left,
        right=emoji_right,
        top=a_emoji_top,
        bottom=a_emoji_bottom,
    )

    b_emoji_top = a_height + legend_height * 3 // 2 - emoji_size // 2
    b_emoji_top -= b_emoji_top % 2
    b_emoji_bottom = merged.height - b_emoji_top - emoji_size

    b_emoji = core.fmtc.bitdepth(
        core.fmtc.resample(b_emoji, emoji_size, emoji_size), bits=8
    )
    b_emoji_mask = core.std.AddBorders(
        blank(b_emoji),
        left=emoji_left,
        right=emoji_right,
        top=b_emoji_top,
        bottom=b_emoji_bottom,
    )
    b_emoji = core.std.AddBorders(
        b_emoji,
        left=emoji_left,
        right=emoji_right,
        top=b_emoji_top,
        bottom=b_emoji_bottom,
    )

    merged = core.std.MaskedMerge(merged, a_emoji, a_emoji_mask)
    merged = core.std.MaskedMerge(merged, b_emoji, b_emoji_mask)
    merged.set_output()
else:
    a = a_horiz
    b = b_horiz
    legend_height = legend_height_horiz

    a_width = a.width
    b_width = b.width
    b_mask = core.std.AddBorders(blank(b), left=a_width, top=legend_height)

    a = core.std.AddBorders(a, right=b_width, top=legend_height)
    b = core.std.AddBorders(b, left=a_width, top=legend_height)

    merged = core.std.MaskedMerge(a, b, b_mask)

    emoji_size = 2 * round(0.8 * legend_height / 2)
    emoji_top = legend_height // 2 - emoji_size // 2
    emoji_top -= emoji_top % 2
    emoji_bottom = merged.height - emoji_top - emoji_size

    a_emoji = core.fmtc.bitdepth(
        core.fmtc.resample(a_emoji, emoji_size, emoji_size), bits=8
    )

    a_emoji_left = max(0, a_width // 2 - emoji_size // 2)
    a_emoji_left += a_emoji_left % 2
    a_emoji_right = merged.width - a_emoji_left - emoji_size
    a_emoji_mask = core.std.AddBorders(
        blank(a_emoji),
        left=a_emoji_left,
        right=a_emoji_right,
        top=emoji_top,
        bottom=emoji_bottom,
    )
    a_emoji = core.std.AddBorders(
        a_emoji,
        left=a_emoji_left,
        right=a_emoji_right,
        top=emoji_top,
        bottom=emoji_bottom,
    )

    b_emoji = core.fmtc.resample(b_emoji, emoji_size, emoji_size, css="4:2:0")
    b_emoji = core.fmtc.bitdepth(b_emoji, bits=8)
    b_emoji_right = max(0, b_width // 2 - emoji_size // 2)
    b_emoji_right -= b_emoji_right % 2
    b_emoji_left = merged.width - b_emoji_right - emoji_size
    b_emoji_mask = core.std.AddBorders(
        blank(b_emoji),
        left=b_emoji_left,
        right=b_emoji_right,
        top=emoji_top,
        bottom=emoji_bottom,
    )
    b_emoji = core.std.AddBorders(
        b_emoji,
        left=b_emoji_left,
        right=b_emoji_right,
        top=emoji_top,
        bottom=emoji_bottom,
    )

    merged = core.std.MaskedMerge(merged, a_emoji, a_emoji_mask)
    merged = core.std.MaskedMerge(merged, b_emoji, b_emoji_mask)
    merged.set_output()
