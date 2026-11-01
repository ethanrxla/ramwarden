#!/usr/bin/env python3
"""Read-only Atlas connectivity check. Requires pymongo; never prints the URI."""
import os
from pathlib import Path
import sys
import tomllib


def load_uri():
    uri = os.environ.get('MONGODB_URI', '').strip()
    secrets = Path.home() / '.config/ramwarden/secrets.toml'
    if not uri and secrets.exists():
        data = tomllib.loads(secrets.read_text())
        uri = data.get('mongodb_uri', '') or data.get('MONGODB_URI', '')
    if not uri:
        for p in (Path('.env'), secrets.parent / '.env'):
            if p.exists():
                for line in p.read_text().splitlines():
                    key, sep, value = line.strip().removeprefix('export ').partition('=')
                    if sep and key.strip() == 'MONGODB_URI':
                        uri = value.strip().strip('\"\'')
                        if uri:
                            break
            if uri:
                break
    if not uri or '<' in uri or not uri.startswith(('mongodb+srv://', 'mongodb://')):
        raise ValueError('MongoDB URI is missing, invalid, or still contains a placeholder.')
    return uri


def main():
    try:
        from pymongo import MongoClient
        uri = load_uri()
        with MongoClient(uri, serverSelectionTimeoutMS=10000, connectTimeoutMS=5000) as client:
            client.admin.command('ping')
        print('Atlas authentication and ping succeeded. No data was written.')
    except ImportError:
        print('Install pymongo in a virtual environment to run this check.', file=sys.stderr)
        return 1
    except ValueError:
        print('MongoDB URI is missing, invalid, or contains a placeholder; check your local secret file.', file=sys.stderr)
        return 1
    except Exception as exc:
        # Driver errors can include usernames, hosts, or credential material.
        print(f'Atlas connection failed ({type(exc).__name__}). Check credentials, DNS, and the Atlas IP access list.', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
