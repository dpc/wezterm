#!/usr/bin/env python3
"""Generate a cell-border demonstration; only write the requested output file."""
import argparse

ESC = "\x1b"
data = []


def emit(text):
    data.append(text)


def at(row, col):
    emit(f"{ESC}[{row};{col}H")


def border(body):
    emit(f"{ESC}P1+bcell-border;{body}{ESC}\\")


def panel(row, col, rows, cols, style, color, radius):
    # Paint backgrounds/text first, then physical-cell borders.
    for y in range(row, row + rows):
        at(y, col)
        emit(f"{ESC}[48;2;35;44;59m" + " " * cols + f"{ESC}[m")
    for y, x, height, width, mask in [
        (row, col, 1, cols, 3),
        (row + rows - 1, col, 1, cols, 48),
        (row, col, rows, 1, 192),
        (row, col + cols - 1, rows, 1, 12),
    ]:
        at(y, x)
        border(f"set;{mask};48;{color};1;1;{height};{width};style={style};priority=1")
    for y, x, joint in [
        (row, col, 1), (row, col + cols - 1, 2),
        (row + rows - 1, col + cols - 1, 4), (row + rows - 1, col, 8),
    ]:
        at(y, x)
        border(f"round;{joint};1;1;outside=default;radius={radius}")
    at(row + 1, col + 2)
    emit(f"{ESC}[38;2;235;240;250;48;2;35;44;59m{style} / radius {radius}{ESC}[m")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, help="destination binary file; never writes to the terminal")
    args = parser.parse_args()
    emit(f"{ESC}[m{ESC}[2J{ESC}[H{ESC}[?25l")
    emit("Cell borders: no character columns consumed")
    for row, col, style, color, radius in [
        (3, 3, "solid", "80c5b7", 256),
        (3, 35, "double", "d5a6ff", 128),
        (10, 3, "dashed", "ffbb77", 64),
        (10, 35, "dotted", "88bbff", 128),
    ]:
        panel(row, col, 5, 28, style, color, radius)
    at(17, 3)
    emit("Pen: ")
    border("pen;255;32;p2;0;0")
    border("pen-round;15;outside=default;radius=128")
    emit("ABCDE")
    emit(f"{ESC}[m  Wide: ")
    border("pen;255;32;p6;1;1")
    emit("界")
    emit(f"{ESC}[m  Centre arms: ")
    border("pen;3840;32;p3;0;1;style=double")
    emit("    ")
    emit(f"{ESC}[m")
    at(20, 3)
    emit("Palette colours follow OSC 4; borders scroll and reflow with cells.")
    at(22, 3)
    emit("Experimental draft v1; SGR 0 clears the pen, not existing borders.")
    emit(f"{ESC}[m")
    with open(args.output, "wb") as output:
        output.write("".join(data).encode("utf-8"))


if __name__ == "__main__":
    main()
