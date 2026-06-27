query 50121 "Customer Balances"
{
    QueryType = Normal;

    elements
    {
        dataitem(Customer; "Test Customer")
        {
            column(No; "No.") { }
            column(Name; Name) { }
            column(Balance; Balance)
            {
                Method = Sum;
            }
        }
    }
}
