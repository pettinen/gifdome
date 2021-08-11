from datetime import datetime, timezone


apos = "\u2019"
emoji_a = "\U0001F170\uFE0F"
emoji_b = "\U0001F171\uFE0F"


def duration(sec):
    hours = sec // 3600
    minutes = (sec - hours * 3600) // 60
    seconds = sec - hours * 3600 - minutes * 60
    parts = []
    if hours:
        parts.append(f"{hours} hour{plural(hours)}")
    if minutes:
        parts.append(f"{minutes} minute{plural(minutes)}")
    if seconds:
        parts.append(f"{seconds} second{plural(seconds)}")
    return " ".join(parts)


def enum_values(enum):
    return {x.value for x in enum}


def ext(mime_type):
    if mime_type == "video/mp4":
        return ".mp4"
    if mime_type == "image/gif":
        return ".gif"
    return ""


def find_enum_by_value(enum, value):
    for x in enum:
        if x.value == value:
            return x
    return None


def markdown_escape(text):
    return re.sub(r"[\\_*\[\]()~`>#+\-=|{}.!]", r"\\\g<0>", text)


def now():
    return int(datetime.now(timezone.utc).timestamp())


def plural(n):
    if n == 1:
        return ""
    return "s"
