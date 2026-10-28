"""Measure this camera's image-to-PTZ mapping, restoring its original pose."""
import json
from pathlib import Path
import sys
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from camera_mcp import Camera
from aiming import track_point


def main():
    camera = Camera()
    camera.stop()
    origin = camera.position()
    baseline = camera.see()
    columns = []
    samples = []
    try:
        for axis, amount in [(0, .04), (1, .12)]:
            target = origin.copy()
            target[axis] += amount if origin[axis]+amount < .95 else -amount
            actual = camera.goto(target)
            after = camera.see()
            shifts = []
            for x in (.3, .5, .7):
                for y in (.3, .5, .7):
                    point = np.array([x, y])
                    tracked = track_point(baseline, after, point)
                    if tracked is not None:
                        shifts.append((tracked-point)/(actual[axis]-origin[axis]))
            if len(shifts) < 4:
                raise RuntimeError('Not enough static texture for calibration; aim at a textured stationary scene')
            column = np.median(shifts, axis=0)
            if abs(column[axis]) < .1:
                raise RuntimeError('Insufficient visual movement; calibration was not saved')
            columns.append(column)
            samples.append(len(shifts))
            camera.goto(origin)
    finally:
        camera.goto(origin)
    matrix = np.column_stack(columns)
    if np.linalg.cond(matrix) > 20:
        raise RuntimeError('Unreliable calibration; previous calibration retained')
    info = camera.device.devicemgmt.GetDeviceInformation()
    data = {'model': info.Model, 'firmware': info.FirmwareVersion,
            'image_shift_per_pose_unit': matrix.tolist(),
            'method': 'Median local ORB/RANSAC point displacement per absolute ONVIF position unit',
            'samples_per_axis': samples,
            'coordinate_system': 'image normalized from top-left; PTZ position as reported by ONVIF'}
    path = ROOT / 'calibration.json'
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(data, indent=2)+'\n')
    temporary.replace(path)
    print(json.dumps(data, indent=2))


if __name__ == '__main__':
    main()
