"""Visual point tracking and empirical image-to-PTZ calibration."""
from io import BytesIO
import json
from pathlib import Path

import cv2
import numpy as np
from PIL import Image


def gray(jpeg):
    image = cv2.imdecode(np.frombuffer(jpeg, np.uint8), cv2.IMREAD_GRAYSCALE)
    if image is None:
        raise ValueError('Invalid camera image')
    return image


def track_point(before, after, point):
    """Track a normalized image point using nearby, RANSAC-consistent ORB features.

    Exclude fixed timestamp/logo overlays. Require a geometrically supported point;
    do not extrapolate from a remote patch when the subject has disappeared.
    """
    a, b = gray(before), gray(after)
    if a.shape != b.shape:
        raise ValueError('Image dimensions changed')
    h, w = a.shape
    mask = np.full(a.shape, 255, np.uint8)
    mask[:int(.07*h), :] = 0
    mask[int(.88*h):, :int(.2*w)] = 0
    orb = cv2.ORB_create(nfeatures=3500, fastThreshold=10)
    ka, da = orb.detectAndCompute(a, mask)
    kb, db = orb.detectAndCompute(b, mask)
    if da is None or db is None:
        return None
    matches = cv2.BFMatcher(cv2.NORM_HAMMING).knnMatch(da, db, k=2)
    good = [pair[0] for pair in matches if len(pair) == 2 and pair[0].distance < .7 * pair[1].distance]
    if len(good) < 12:
        return None
    src = np.float32([ka[m.queryIdx].pt for m in good])
    dst = np.float32([kb[m.trainIdx].pt for m in good])
    pixel = np.array(point) * [w, h]
    distances = np.linalg.norm((src - pixel) / [w, h], axis=1)
    nearby = distances < .25
    if nearby.sum() < 12:
        return None
    src, dst = src[nearby], dst[nearby]
    matrix, inliers = cv2.findHomography(src, dst, cv2.RANSAC, 3)
    if matrix is None or inliers is None or inliers.sum() < 10 or inliers.mean() < .4:
        return None
    supported = src[inliers.ravel().astype(bool)]
    if np.min(np.linalg.norm((supported - pixel) / [w, h], axis=1)) > .12:
        return None
    transformed = cv2.perspectiveTransform(np.float32([[pixel]]), matrix)[0, 0] / [w, h]
    if not np.all(np.isfinite(transformed)) or np.any(transformed < 0) or np.any(transformed > 1):
        return None
    # Reject degenerate fits that collapse or magnify the image dramatically.
    neighbors = np.float32([[pixel, pixel+[10, 0], pixel+[0, 10]]])
    mapped = cv2.perspectiveTransform(neighbors, matrix)[0]
    scale = np.linalg.norm(mapped[1:] - mapped[0], axis=1) / 10
    if np.any(scale < .4) or np.any(scale > 2.5):
        return None
    return transformed


def load_calibration(path):
    data = json.loads(Path(path).read_text())
    matrix = np.asarray(data['image_shift_per_pose_unit'], dtype=float)
    if matrix.shape != (2, 2) or not np.all(np.isfinite(matrix)) or np.linalg.cond(matrix) > 20:
        raise ValueError('Invalid camera calibration')
    return matrix


def image_size(jpeg):
    with Image.open(BytesIO(jpeg)) as image:
        return image.size
