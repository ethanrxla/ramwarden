#!/usr/bin/env python3
"""Operator-only complimentary plan testing; no public route or wallet contract.
Requires MONGODB_URI, MONGODB_DATABASE and RAMWARDEN_DEVELOPER_ACCOUNT_ID.
The account allowlist belongs in private operator configuration, never the UI.
"""
import argparse
import os
import time
from pymongo import MongoClient

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--account-id', required=True)
parser.add_argument('--expected-plan', choices=['lite', 'plus', 'pro'], required=True)
parser.add_argument('--plan', choices=['lite', 'plus', 'pro'], required=True)
parser.add_argument('--reason', required=True)
args = parser.parse_args()
assert args.account_id == os.environ.get('RAMWARDEN_DEVELOPER_ACCOUNT_ID'), 'Not the configured developer account'
assert len(args.reason.strip()) >= 10, 'An audit reason is required'
db = MongoClient(os.environ['MONGODB_URI'], serverSelectionTimeoutMS=10000)[os.environ['MONGODB_DATABASE']]
db.accounts.create_index('developer_access', unique=True, partialFilterExpression={'developer_access': True}, name='single_developer_account')
now = int(time.time())
def apply(session):
    account = db.accounts.find_one({'_id': args.account_id}, session=session)
    assert account, 'Account not found'
    assert not account.get('billing'), 'Do not overwrite a real subscription'
    assert not db.payments.find_one({'account_id': args.account_id, 'status': 'pending'}, session=session), 'Resolve pending checkout first'
    active = (account.get('paid_until') or 0) > now
    current = {'ram':'plus', 'pro':'pro'}.get(account.get('tier'), 'lite') if active else 'lite'
    assert current == args.expected_plan, 'Account plan changed; review before retrying'
    fields = dict(developer_access=True, tier={'lite':None,'plus':'ram','pro':'pro'}[args.plan],
                  paid_until=now+30*86400 if args.plan != 'lite' else None,
                  interval=None, trial_ends_at=None, cancelled=False)
    db.developer_access_audit.insert_one(dict(account_id=args.account_id, at=now, reason=args.reason,
        previous={k:account.get(k) for k in fields}, granted=fields), session=session)
    result = db.accounts.update_one({'_id':args.account_id}, {'$set':fields}, session=session)
    assert result.matched_count == 1
with db.client.start_session() as session:
    session.with_transaction(apply)
print(f'Developer {args.plan} access set for 30 days. No payment created. Audit retained.')
