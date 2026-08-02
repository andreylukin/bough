def business_days(start, end):
    """Weekdays in [start, end], both inclusive. Empty when end < start."""
    from datetime import timedelta

    days = 0
    day = start
    while day < end:
        if day.isoweekday() <= 5:
            days += 1
        day += timedelta(days=1)
    return days
