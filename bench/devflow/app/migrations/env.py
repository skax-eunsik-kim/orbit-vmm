import os

from alembic import context
from sqlalchemy import create_engine

engine = create_engine(os.environ["DATABASE_URL"])
with engine.connect() as conn:
    context.configure(connection=conn)
    with context.begin_transaction():
        context.run_migrations()
