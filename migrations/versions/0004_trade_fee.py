"""trades add fee

Revision ID: 0004
Revises: 0003
Create Date: 2025-01-04

"""
import sqlalchemy as sa
from alembic import op

revision = "0004"
down_revision = "0003"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.add_column("trades", sa.Column("fee", sa.Float(), nullable=False, server_default="0"))


def downgrade() -> None:
    op.drop_column("trades", "fee")
