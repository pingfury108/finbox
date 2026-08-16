"""add screenings

Revision ID: 0003
Revises: 0002
Create Date: 2025-01-03

"""
import sqlalchemy as sa
from alembic import op

revision = "0003"
down_revision = "0002"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "screenings",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.Column("symbol", sa.String(16), nullable=False),
        sa.Column("name", sa.String(64), nullable=False),
        sa.Column("reason", sa.String(128), nullable=False),
        sa.Column("metrics", sa.Text(), nullable=False),
    )
    op.create_index("ix_screenings_ts", "screenings", ["ts"])


def downgrade() -> None:
    op.drop_index("ix_screenings_ts", table_name="screenings")
    op.drop_table("screenings")
