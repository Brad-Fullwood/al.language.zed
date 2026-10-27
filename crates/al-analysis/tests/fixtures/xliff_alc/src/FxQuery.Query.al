query 50100 "FX Query"
{
    Caption = 'FX Query';

    elements
    {
        dataitem(Cust; Customer)
        {
            column(CustNo; "No.")
            {
                Caption = 'Query No';
            }
            filter(CustFilter; Name)
            {
                Caption = 'Query Filter';
            }
        }
    }
}
