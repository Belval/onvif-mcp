from unittest.mock import Mock, patch
import pytest
from camera_mcp import Camera, safe_call


def fake_camera():
    c = Camera.__new__(Camera)
    c.ptz = Mock()
    c.profile = Mock(token='profile')
    return c


@pytest.mark.parametrize('values', [(2,0,.1),(0,-2,.1),(0,0,3),(0,0,0),(float('nan'),0,.1)])
def test_reject_unsafe_movement(values):
    c = fake_camera()
    with pytest.raises(ValueError):
        c.move(*values)
    c.ptz.ContinuousMove.assert_not_called()


def test_stop_even_when_move_fails():
    c = fake_camera()
    c.ptz.ContinuousMove.side_effect = RuntimeError('network failure')
    with pytest.raises(RuntimeError):
        c.move(.1, 0, .1)
    c.ptz.Stop.assert_called_once()


def test_camera_timeout_and_explicit_stop():
    c = fake_camera()
    with patch('camera_mcp.time.sleep'):
        c.move(.1, -.1, .3)
    request = c.ptz.ContinuousMove.call_args.args[0]
    assert request['Timeout'].total_seconds() == 1
    c.ptz.Stop.assert_called_once()


def test_errors_do_not_expose_credentials():
    with patch('camera_mcp.camera', side_effect=RuntimeError('rtsp://secret:password@camera')):
        with pytest.raises(RuntimeError) as err:
            safe_call('see')
    assert 'secret' not in str(err.value)
    assert 'password' not in str(err.value)
