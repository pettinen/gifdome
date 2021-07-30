def generate_seeding(ordered_submissions):
    assert len(ordered_submissions) == 256

    output = []
    for i in range(128):
        output.append(ordered_submissions[i])
        output.append(ordered_submissions[255 - i])
    return output


def bracket_coordinates():
    coords = {}
    for i in range(128, 160):
        coords[i] = (82, 82 + (i - 128) * 140)
    for i in range(160, 192):
        coords[i] = (6334, 82 + (i - 160) * 140)
    for i in range(192, 208):
        coords[i] = (464, 152 + (i - 192) * 280)
    for i in range(208, 224):
        coords[i] = (5952, 152 + (i - 208) * 280)
    for i in range(224, 232):
        coords[i] = (846, 292 + (i - 224) * 560)
    for i in range(232, 240):
        coords[i] = (5570, 292 + (i - 232) * 560)
    for i in range(240, 244):
        coords[i] = (1099, 508 + (i - 240) * 1120)
    for i in range(244, 248):
        coords[i] = (5189, 508 + (i - 244) * 1120)
    for i in range(248, 250):
        coords[i] = (1611, 1068 + (i - 248) * 2240)
    for i in range(250, 252):
        coords[i] = (4677, 1068 + (i - 250) * 2240)
    coords[252] = (2180, 1528)
    coords[253] = (3852, 2592)
    coords[254] = (3016, 2060)

    sizes = {}
    for i in range(128, 240):
        sizes[i] = 128
    for i in range(240, 252):
        sizes[i] = 256
    for i in range(252, 255):
        sizes[i] = 512

    return coords, sizes
